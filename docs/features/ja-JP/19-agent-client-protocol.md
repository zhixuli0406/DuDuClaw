# ACP と A2A のインターフェース

DuDuClaw は IDE の会話と外部からのタスク送信に別々のインターフェースを提供します。トランスポート、認証情報、タスクの寿命は異なります。

| インターフェース | 入口 | 用途 |
|---|---|---|
| Agent Client Protocol v1 | `duduclaw acp`、stdio | IDE セッションとプロンプトのターン |
| 既存の A2A コマンド | `duduclaw acp server`、stdio | `agent/discover`、`message/send`、`tasks/send` / `tasks/get` / `tasks/cancel` を使うローカルスクリプト |
| A2A v1.0 JSON-RPC、v1.80.0 から | Gateway `POST /a2a` | 認証済みクライアントによるタスクボードの目標作成 |
| MCP | `duduclaw mcp-server` または独立した HTTP server | AI runtime と MCP client 向けのツール |

## Gateway A2A v1.0（v1.80.0 から）

Gateway は `/.well-known/agent-card.json` に署名付き Agent Card を公開します。JSON-RPC インターフェース、プロトコルバージョン `1.0`、管理者が明示的に公開した社員の実際に有効なスキル、Dashboard Bearer JWT 認証を記載します。署名はカードの完全性を確認するもので、呼び出し元への権限付与にはなりません。ストリーミングやプッシュ通知の対応は宣言しません。名前と説明が空でない有効なスキルのみ公開し、実際の社員を示す `employee:<id>` タグを付けます。公開社員や公開可能なスキルがない場合、公開カードは HTTP 503 を返します。認証済みの呼び出し元に利用可能なスキルがなければ、`GetExtendedAgentCard` は `-32007` を返します。`SendMessage` の対象は、公開可能なスキルが少なくとも一つある公開社員に限ります。署名鍵が利用できない場合、探索エンドポイントは HTTP 503 を返し、未署名のカードは提供しません。

初期状態では社員を公開しません。`config.toml` に既存の社員 id を指定します。

```toml
[a2a]
exposed_agents = ["researcher"]
public_url = "https://host.example/a2a"
trusted_proxy_cidrs = []
```

`public_url` は外部から到達できる `/a2a` の HTTPS URL です。TLS は gateway の前に置くリバースプロキシで設定します。公開リストは既存の認可を制限するだけで、Dashboard アカウントに社員へのアクセスを付与したり、無効なスキルを有効化したり、ツール権限を変更したりしません。空のリスト、読み取れない設定、不正な A2A 設定ではタスクへのアクセスを拒否します。社員をリストから外すと、その社員の以前のタスクも A2A 経由ではアクセスできなくなります。

`trusted_proxy_cidrs` の初期値は `[]` で、有効な IPv4／IPv6 CIDR を最大 32 件指定できます。管理下の直接接続プロキシだけを設定してください。一致したプロキシは `X-Forwarded-For` を単一の client IP に上書きする必要があります。欠落、重複、IP の連鎖、不正値は拒否します。それ以外の接続元が送るヘッダーは無視します。未設定では、同じプロキシ配下の client が通信種別ごとにプロキシ IP の枠を共有します。例えば `192.0.2.10` が直接接続プロキシの場合に限り、`trusted_proxy_cidrs = ["192.0.2.10/32"]` を指定します。この設定は呼び出し権限を付与しません。

各 RPC に `Authorization: Bearer <Dashboard access JWT>` と `A2A-Version: 1.0` が必要です。JWT は既存の Dashboard アカウントログインで取得します。旧共有管理者 token や MCP HTTP-server API key は受け付けません。公開カードは認証なしで取得できますが、`GetExtendedAgentCard` は認証が必要で、アクセス制限も維持されます。

バージョンヘッダーがない場合や空の場合は `0.3` と見なします。この HTTP インターフェースは `0.3` に対応せず、`VersionNotSupportedError`（`-32009`）を返します。v1.0 の PascalCase メソッドを使用してください。既存の stdio 名 `message/send` と `tasks/get` は HTTP の別名として残りますが、`1.0` ヘッダーと v1.0 リクエストオブジェクトが必要です。0.3 トランスポートへの対応ではありません。

### 送信とポーリング

次の例は実際の gateway では未検証です。ホスト名と社員 id を置き換え、有効な Dashboard JWT を `DUDUCLAW_DASHBOARD_JWT` に設定してください。

```sh
curl --fail-with-body 'https://host.example/a2a' \
  -H "Authorization: Bearer $DUDUCLAW_DASHBOARD_JWT" \
  -H 'A2A-Version: 1.0' \
  -H 'Content-Type: application/json' \
  --data '{"jsonrpc":"2.0","id":1,"method":"SendMessage","params":{"metadata":{"agentId":"researcher"},"message":{"messageId":"request-001","role":"ROLE_USER","parts":[{"text":"Summarize the public project documentation."}]},"configuration":{"returnImmediately":true}}}'
```

`metadata.agentId` で公開された社員を選択します。`message.messageId` は必須、`role` は `ROLE_USER`、`parts` はプレーンテキストのみです。`configuration.returnImmediately = true` を指定します。既存のタスクボード目標作成経路で非同期の目標を作成し、`result.task` を返します。実行には社員の runtime 設定と既存の goal loop の権限チェックを使用します。送信には直接の Operator 権限または Admin が必要で、チーム経由の権限だけでは送信できません。メッセージは最大 4,000 文字、HTTP 本文は最大 64 KiB です。新規タスクのみ受け付けるため、`message.contextId` と `message.taskId` は省略してください。返される `contextId` は task id と同じです。`historyLength` は省略または `0` のみです。認証済みアカウントと `messageId` を永続的な一意キーとして扱います。同じ完全な `params` を再送すると元のタスクを返し、同じ id で異なるパラメーターを送ると拒否します。JSON オブジェクトのキー順序は判定に影響しません。タスクを削除しても id は再利用できず、再送による再作成も行いません。再送時も現在の認可が必要です。

重複判定は正規化した完全な `params` を比較します。オブジェクトのキー順序は無視し、配列の順序は保持します。フィールドの省略と既定値の明示指定は異なるパラメーターとして扱います。内容の競合は `-32602`（`INVALID_ARGUMENT`）、削除後の再送は `-32001`（`TASK_NOT_FOUND`）です。ページ token は所有者と context・status・status timestamp・artifacts の条件に結び付けられ、`pageSize` はページごとに変更できます。

返された task id を使い、同じ認証・バージョンヘッダーで同じエンドポイントに送信します。

```json
{"jsonrpc":"2.0","id":2,"method":"GetTask","params":{"id":"<task-id>"}}
{"jsonrpc":"2.0","id":3,"method":"ListTasks","params":{"pageSize":20}}
{"jsonrpc":"2.0","id":4,"method":"CancelTask","params":{"id":"<task-id>"}}
{"jsonrpc":"2.0","id":5,"method":"GetExtendedAgentCard","params":{}}
```

`ListTasks` は `contextId`、`status`、`statusTimestampAfter`、`includeArtifacts` に対応します。`pageSize` は 1〜100 で、不透明な `pageToken` カーソルを使用します。次のページには `nextPageToken` を渡します。タスクは単調増加する作成連番で並び、最初のページで固定した上限を後続の token に引き継ぎます。ページ取得中に作成されたタスクを表示するには、token なしで新しい一覧を開始してください。権限と状態の条件は毎ページ再確認するため、固定されるのは対象範囲であり、タスクの状態ではありません。`totalSize` は現在閲覧可能なタスクだけを数えます。単一の呼び出し元に保存されたタスクが 10,000 件を超える場合、不完全な総数を返さず `RESOURCE_EXHAUSTED` を返します。読み取りと取消は、同じ認証済みアカウントが A2A 経由で作成したタスクだけに限ります。社員の公開状態、現在のアカウントと社員への権限、タスクの閲覧対象も再確認します。所有者はタスクに永続保存され、子タスクにも引き継がれます。通常の Dashboard のタスク一覧・読み取り・編集・削除・停止・コメント・添付・活動記録・プッシュイベントにも同じ所有者制限を適用します。Admin の例外や別の管理者上書き経路はありません。削除後も所有者記録を保持し、残存する証跡を保護します。Dashboard 所有者の主体を持たない外部 MCP 呼び出し元は拒否し、認証済みで実行担当に指定された社員には必要なタスクアクセスを残します。

状態は `TASK_STATE_SUBMITTED`、`TASK_STATE_WORKING`、`TASK_STATE_COMPLETED`、`TASK_STATE_FAILED`、`TASK_STATE_INPUT_REQUIRED`、`TASK_STATE_CANCELED` などの `TASK_STATE_*` を使います。承認や人の入力を待つ状態は完了ではありません。取消には既存のタスクツリー停止機構を使用します。停止が保留中または不確かな場合はポーリングを続けてください。外部プロセスや副作用が物理的に停止した証明にはなりません。終了状態のタスクを取り消すと `TaskNotCancelableError`（`-32002`）を返します。

### 対応範囲とエラー

`forward.recent` と `forward.chain` は削除済み A2A タスクを含め、保持された所有者記録を確認します。Admin も他の呼び出し元の所有権を回避できません。`tasks.flow_metrics` は他の A2A 所有者のタスクを除外してから件数・時間・ラウンド数を集計します。A2A 以外のタスクのアクセス規則は変わりません。

タスク DB のスキーマ移行前に、A2A 構造や再送記録が存在する場合は、読み取り可能な `task_access_principals` テーブルを必須とします。欠落または読み取り不能なら DB を開かず、残存タスクから空の登録表を再作成しません。一貫したバックアップから完全な所有者記録を復元してください。新規 DB と A2A の痕跡がない旧 DB は引き続き移行できます。

既存の `keys/a2a-signing.ed25519` はシンボリックリンクではない通常ファイルである必要があります。Unix では gateway の実効ユーザーが所有し、モードが正確に `0600` であることも確認します。Windows では Unix の所有者・モード確認を省略します。不正なファイルは上書きや権限の自動変更をせず拒否します。接続元 IP の枠を先に確認・記録し、通過した要求だけがグローバル枠を消費します。接続元で拒否された要求は他の接続元向けの容量を消費しません。

| 操作 | 対応範囲 |
|---|---|
| `SendMessage` | 新しいプレーンテキストの非同期目標 |
| `GetTask`、`ListTasks` | 自分が作成し、現在もアクセス可能な A2A タスク |
| `CancelTask` | 自分のタスクツリーへの停止要求 |
| `GetExtendedAgentCard` | 認証付きカード。追加の権限なし |
| `SendStreamingMessage`、`SubscribeToTask` | 非対応。SSE なし |
| 完了まで待機する応答、既存タスクの継続、プッシュ通知設定 | 非対応 |

JSON-RPC のプロトコルエラーには標準コードを使います。A2A エラーは該当する `-32001` から `-32009` を使い、`error.data` は型付き `google.rpc.ErrorInfo` の配列です。未認証・未認可のリクエストには HTTP 401/403 を使用します。タスク操作前に入力スキャン、メソッド認可、安全監査を適用します。公開探索、認証失敗、認証済み RPC は、それぞれ独立した IP ごとの毎分 120 件の枠を使います。全体の上限も分離し、順に毎分 600、600、1,200 件です。RPC には呼び出し元ごとの毎分 60 件の制限も適用します。匿名の通信は認証済み RPC の枠を消費しません。各 IP／呼び出し元 limiter は最大 10,000 キーを保持し、満杯では新規キーを拒否します。期限切れキーの削除にも実行頻度の上限があります。HTTP 専用で、WebSocket の `a2a.*` 呼び出しは拒否します。[セキュリティ防御](05-security-defense.md)も参照してください。

## IDE Agent Client Protocol

IDE の agent panel には `duduclaw acp` を使います。`initialize`、`session/new`、`session/prompt`、`session/update`、`session/cancel` に対応します。未設定の home では `AUTH_REQUIRED` を返し、端末での onboarding を案内します。プロンプトのターンは gateway の応答パイプラインを通り、Main ロールの社員が処理します。

Zed の設定：

```json
{"agent_servers":{"DuDuClaw":{"command":"duduclaw","args":["acp"]}}}
```

Gateway A2A HTTP メソッドと IDE のセッションメソッドは異なります。IDE panel に `duduclaw acp server` を指定しないでください。

## 既存の stdio A2A

`duduclaw acp server` は既存のローカルプロトコルを維持します。`message/send` は `a2a-client` として gateway の委譲キューに書き込み、既存の `[acp] trusted` ポリシーが適用されます。プロセス内のタスク索引とキューの観測は、HTTP の永続タスクボード目標とは別です。stdio プロセスを再起動すると索引の task id は失われます。キューに応答がなくても完了を証明できません。旧 `tasks/send` のメモリー内管理も HTTP タスクボードとは別です。

このページの HTTP 公開リストと JWT 所有権ルールは `/a2a` に適用され、stdio コマンドの信頼モデルは変更しません。旧版では HTTP を将来の探索専用機能と記載し、未実装のストリーミングや複数ターンのタスク伝送を宣伝していました。HTTP の対応範囲は上表のとおりです。
