# MCP HTTP 操作ごとの権限の有効期間

更新日：2026-10-25。状態：`mcp-client` feature を使う、`duduclaw-llm` の未リリース開発者 API。

ホストは MCP のツール探索や一回のツール呼び出しに、新しい bearer とその操作だけが所有する lease を渡せます。本書は共通トランスポート API のリファレンスです。以下の bearer／lease API は従来の Generic 経路です。未リリースの Google 個人アカウント連携は下記の独立した Closed 経路を使い、新しい画面スイッチは追加しません。

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

## Google ホストの Closed 接続（未リリースのソース草稿）

`RegistryClient::scoped_http_closed(recipe, host_binding, guard)` で明示的に選択します。実際の slot が server、recipe、filter、元の設定項目、絶対期限を持つ `ClosedClientTarget` を非公開コンストラクターで作ります。`prepare_closed_client_call` は一度だけ消費する `OwnedScopedOperation` を返し、既定では拒否します。応答型や最終引数が違う場合、Generic bearer／lease API へ切り替えません。

Gateway はメタデータ処理の移譲前に実際の依頼者を取得し、元の設定項目と順番、完全な frame、操作 nonce、元のアカウント選択を固定します。資格情報所有者が 3 POST、JSON／SSE 解読、文字結合、既知の JSON 再解析、整理、最後の bearer／session 除去を完了し、型付き完了結果だけを渡します。Closed リクエストと各 POST 応答はそれぞれ 16 MiB が上限で、応答は操作全体の合算制限ではありません。Closed の資格情報更新と送信では環境 proxy、リダイレクト、再試行を無効にします。

同じ期限を registry と外側 loop の結果 hook、ソース検証、CCR 準備、次のモデル要求への引き渡しまで使います。期限切れの内容、アカウント通知、新しい CCR 参照は一緒に保留します。`ToolOutcome` の新しい任意フィールドは crate 内限定なので、`mcp-client` 使用時の外部 Rust struct literal はコンストラクターへの移行が必要です。既存コンストラクターの既定は Closed policy なしで、Generic／Persistent の従来シグネチャは維持します。

同期キーチェーンとローカル処理は強制中断できず、戻った時点と次の送信前に期限を再確認します。中断は開始済みの資格情報更新、遠隔作用、保存を取り消しません。CCR は既存の単一呼び出しメタデータ tombstone 取引で清理し、開始済みの有限な SQLite 処理が後で完了する場合があります。クラッシュや清理失敗では未公開行が既存の期限処理まで残り得ます。永続的なタイムアウト調整機構や保存ゼロの保証は追加しません。

新しい M1 テストはソースのみで未コンパイル／未実行です。feature 無効のビルド、独立コード／セキュリティ審査、完全な CI、実プロバイダーと利用者の受入れは未完了です。サービスアカウント委任、Bridge、その他 W3 経路は別作業です。


Closed は provenance と結果メタデータを一時保持し、同期 ledger／route 構築後に元の期限をすべて再確認します。期限切れの結果を除外し、その span を含めず ledger を再構築してから確定します。テスト版では Home 更新端点の独立した owned fixture が必要で、欠ける場合は復号やプロバイダー送信前に拒否します。Legacy の端点方針は変更しません。

## ソース参照

- [MCP client と registry](../../../crates/duduclaw-llm/src/mcp_client.rs)
- [ホスト interceptor と権限型](../../../crates/duduclaw-llm/src/tool_loop/interceptor.rs)
- [Scoped HTTP テスト](../../../crates/duduclaw-llm/src/mcp_client/scoped_http_tests.rs)

[English](../mcp-call-lifetimes.md) · [繁體中文](../zh-TW/mcp-call-lifetimes.md)

### W3 アカウント種別

未公開W3ソースは Home SA と委任を同じClosed操作契約に接続し、Google承認wireにサービスアカウント／Apps Scriptタグと任意の型付き `home_source` を追加します。既存Personal/OAuth JSONはこのフィールドを省略したままです。Rust struct literalで `GoogleAccountApproval` を作る外部コードは `home_source` の追加が必要です。Rustソース互換性の変更であり、公開構造体が同じバイト列であるとは主張しません。新たな依存、token schema、承認gateはありません。W3テストは未コンパイル・未実行です。
