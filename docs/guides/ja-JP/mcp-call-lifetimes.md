# MCP HTTP 操作ごとの権限の有効期間

更新日：2026-10-24。状態：`mcp-client` feature を使う、`duduclaw-llm` の未リリース開発者 API。

ホストは MCP のツール探索や一回のツール呼び出しに、新しい bearer とその操作だけが所有する lease を渡せます。本書は共通トランスポート API のリファレンスです。この変更だけでは Gateway や Google 個人アカウントの処理に接続されず、利用者向けの画面や設定スイッチもありません。

## ホスト API の契約

| API | 契約 |
|---|---|
| `ScopedHttpRecipe::new(url, headers, operation_timeout)` | 不変の接続メタデータ。HTTPS を受け付け、HTTP は解析後に localhost または loopback となるアドレスに限定します。URL の認証情報／fragment、`Authorization`／`Proxy-Authorization`／`Cookie`／`Cookie2`／`Mcp-Session-Id` ヘッダー、不正なヘッダー、ゼロまたは 300 秒を超えるタイムアウトを拒否します。bearer と MCP session は保存しません。信頼できる設定からホストが構築します。 |
| `RegistryClient::scoped_http(recipe, guard)` | 接続メタデータをホスト所有の `Arc<dyn ToolInterceptor>` と組み合わせます。 |
| `ToolRegistry::from_client_slots_named(clients, filters)` | `Persistent` と scoped slot を混在できます。scoped の名前は空白不可です。探索前に guard を実際の client index に固定し、論理名やリモート metadata では差し替えられません。 |
| `ToolInterceptor::prepare_client_call(server, operation, args)` | `ClientOperation::Discover` または `ClientOperation::Tool { name }` を準備します。`PreparedClientCall` に最終引数、`Some(lease)`、`Some(fresh_http_bearer)` を返し、権限フィールドのどちらかが欠ければ拒否します。 |
| `ToolCallLease::validate_before_send(&mut self)` | 読み取り専用で、繰り返し呼べる現在の権限チェックです。認証情報の更新や別の lease の取得をしてはいけません。 |
| `SecretBearer::new(value)` | 今回の新しい bearer を所有します。`Debug`、`Clone`、シリアライズの実装はありません。 |

`before_call` または `before_call_async` だけを実装した既存 interceptor は scoped slot を許可できません。既定の準備処理は探索を拒否し、ツールにも bearer と lease を渡しません。guard がある client index に別の guard を登録することも拒否されます。

## 操作の有効期間

ツール探索は `initialize`、initialized 通知、`tools/list` より前に専用の権限を取得します。ツール呼び出しは対象 slot の mutex を待ち、権限を準備して、新しい HTTP client と MCP session を作成します。同一の bearer と lease が今回の handshake とツール要求を覆い、次の操作には残りません。

初期化と通知を含むすべての POST は、送信直前に lease を検証します。応答の解析後、内容を返す前にも再検証し、通知が受理された後も確認します。この検証で別のアカウントを選び直すことはありません。ホストは lease が検証する権限と、準備時に返した bearer が対応することを保証します。

操作タイムアウトは slot の待機、準備、handshake、HTTP 応答、registry の結果処理まで含みます。探索も準備と通信を含むタイムアウトがあります。完了、エラー、タイムアウト、キャンセルのいずれでも、今回の client と lease は Drop されます。HTTP redirect と retry は無効で、結果が未確認の場合に自動再接続や再実行は行いません。操作をまたいで同じ session を必要とするサーバーは、この scoped モードでは扱えません。

## 結果と制約

準備に失敗すると固定の拒否メッセージを返します。通信、応答の再検証、タイムアウトの失敗は固定の「結果未確認」メッセージとなり、リモートですでに実行された可能性があります。slot 待機中のタイムアウトは送信済みを意味しません。ローカル future の Drop はローカル権限を解放するだけで、リモートの書き込みを取り消した証明にはなりません。未確認の操作を自動で再試行してはいけません。

Scoped の応答は、今回の bearer を返却 JSON の key と value からマスクします。外部サービスの生のエラー本文や準備処理の任意のエラー文字列は返しません。`SecretBearer` は Drop 時に自分の buffer を可能な範囲で上書きしますが、呼び出し元、allocator、HTTP library にあるコピーの消去は保証しません。

`RegistryClient::Persistent` は既存の接続と `before_call_async` 経路を維持します。scoped の準備はその slot の before hook を置き換え、二重には呼びません。既存の filter、同名ツールの先着優先、`after_call`、ソース証明／CCR、ホスト通知は registry の処理を通ります。

## ソース参照

- [MCP client と registry](../../../crates/duduclaw-llm/src/mcp_client.rs)
- [ホスト interceptor と権限型](../../../crates/duduclaw-llm/src/tool_loop/interceptor.rs)
- [Scoped HTTP テスト](../../../crates/duduclaw-llm/src/mcp_client/scoped_http_tests.rs)

[English](../mcp-call-lifetimes.md) · [繁體中文](../zh-TW/mcp-call-lifetimes.md)
