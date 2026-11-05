# AG-UI フロントエンドエンドポイント

外部フロントエンドは `POST /api/ag-ui/agents/{agent_id}/runs` で AI スタッフにメッセージを送り、AG-UI 1.0 のイベントストリームを受信できます。実装範囲は以下の HTTP/SSE サブセットです。32 種類すべてのイベントに対応するものではありません。

v1.80.0 から利用できます。

## メッセージを送る

ダッシュボードアカウントの access JWT を `Authorization: Bearer …` に指定します。匿名ウィジェット、widget key、従来の共有管理者トークンは利用できません。アカウント状態、パスワード変更要求、認証情報の失効、既存の二要素認証ポリシーが適用されます。

`support` をアカウントが呼び出せるスタッフに置き換えてください。Admin 以外は既存のスタッフ割り当てまたは有効なチーム権限が必要で、このエンドポイントからメイン／デフォルトスタッフを呼び出せません。Admin は読み込み済みで稼働可能なスタッフを選択できます。実行中は 500 ミリ秒ごとに JWT とスタッフ権限を再確認し、期限切れや失効時は開いているストリームに `RUN_ERROR` を返します。

```bash
# 通常のログインで取得した access JWT を DUDUCLAW_ACCESS_TOKEN に設定します。
curl --no-buffer 'http://127.0.0.1:18889/api/ag-ui/agents/support/runs' \
  -H "Authorization: Bearer ${DUDUCLAW_ACCESS_TOKEN}" \
  -H 'Content-Type: application/json' \
  -H 'Accept: text/event-stream' \
  --data-binary '{
    "threadId": "support-chat-1",
    "runId": "run-1",
    "protocolVersion": "1.0",
    "messages": [{"id":"user-1","role":"user","content":"できることを簡単に紹介してください。"}],
    "tools": [],
    "context": [],
    "forwardedProps": {}
  }'
```

同じ会話では `threadId` を維持し、リクエストごとに新しい `runId` を使います。Gateway は認証済みの呼び出し元、スタッフ、thread から会話キーを生成します。別のアカウントが thread ID を推測しても同じ履歴にはアクセスできません。セッションは既存の WebChat 所有者名前空間に独立した AG-UI 区画を持ち、`/ws/chat` からの再開は拒否します。

既存の WebChat 応答処理へ渡すのは最後の user メッセージだけです。内容は文字列または text parts の配列を指定できます。クライアントの system メッセージ、assistant 履歴、`tools`、`context`、`state`、`forwardedProps` はサーバー側の会話を置き換えず、プロンプトにも追加しません。これらのフィールドは受け付けますが、フロントエンド定義のツールを登録・実行しません。スタッフが既存 runtime のツールを実際に要求した場合に限り、対応する名前が進捗に現れます。それ以外の client tool 宣言は無視します。client state と forwarded properties はイベントへ返しません。

## ストリームを読む

各 SSE `data:` フレームは一つの JSON イベントです。通常の応答順序は次のとおりです。

```text
RUN_STARTED（protocolVersion: "1.0"）
[TOOL_CALL_START → TOOL_CALL_END、ツール進捗がある場合]
TEXT_MESSAGE_START
TEXT_MESSAGE_CONTENT
TEXT_MESSAGE_END
RUN_FINISHED（outcome.type: "success"）
```

現在の応答処理は検査済みの最終テキストを返すため、既存の出力・配信チェック後に一つの content chunk を送ります。ツール進捗には観測したツール名と生成した call ID が含まれます。生の引数やツール結果は公開せず、`TOOL_CALL_ARGS` や `TOOL_CALL_RESULT` を捏造しません。進捗が得られるかはスタッフの runtime に依存します。

JSON、入力形式、認証、スタッフ権限のエラーはストリーム開始前に拒否します。`RUN_STARTED` 後の失敗は `RUN_ERROR` で通知します。HTTP 200 だけでは成功と判断できません。終了イベントなしで接続が閉じた場合はストリームが途切れています。応答処理が通知した runtime の失敗も `RUN_ERROR` に変換し、成功したテキスト応答として扱いません。

## 承認と再開

サーバーが所有するこの会話とターンに属する承認待ち要求は、`RUN_FINISHED`、`outcome.type: "interrupt"`、`interrupts` 項目として返されます。理由は `confirmation` で、承認 ID、有効期限、このエンドポイントで決定できるかを示す `metadata["duduclaw:canDecide"]` を含みます。中断の前に state と message の snapshot を送ります。内容はエンドポイントの再開情報であり、任意の client state を返すものではありません。

同じ thread で新しい run を送信し、未回答の中断をすべて `resume` に含めます。以下の ID は受信した承認 ID に置き換えてください。

```json
{
  "threadId": "support-chat-1",
  "runId": "run-2",
  "protocolVersion": "1.0",
  "messages": [],
  "resume": [{
    "interruptId": "approval-id-from-interrupt",
    "status": "resolved",
    "payload": {"approved": true}
  }]
}
```

拒否は `{"approved": false}` です。`status: "cancelled"` も拒否を意味し、`payload` を省略する必要があります。`editedArgs` は未対応です。欠落、余分、重複、期限切れ、無関係な中断 ID は拒否します。通常の新しいメッセージで未回答の中断を回避することはできません。一つの resume の決定は単一トランザクションで確定します。競合、要求内容の変更、発生元の失効、対象の不足がある場合、そのリクエストの決定をすべてロールバックします。新しい run ID で再試行しても、以前の成功したリクエストで確定した決定は変更できません。項目の順序と metadata は決定の同一性に影響しません。

ここで決定できるのは、有効な Admin による通常の、特定の実行主体に束縛されていない `mcp_call` または `mcp_install` 承認だけです。ダッシュボード専用とその他の承認は通知のみで、元の認可された画面から決定します。その画面で承認または拒否した後、新しい run ID で再接続し、前回通知されたすべての中断（外部で決定済みのものも含む）を resume に指定します。approved の真偽値は保存済みの結果と一致させてください。Gateway は元の要求、ターンの発生元、保存済みの決定を確認してから元の応答処理を再開します。現在もスタッフへのアクセス権がある利用者は、Admin 以外でも既存の決定を確認できます。結果や決定者は変更されません。未決定カードには元の決定権限が必要です。期限切れ、無効化済み、結果の不一致は再開を拒否します。期限内に決定済みなら、カード期限後も元のターンの有効期間内に確認できます。`forbid_self_approval` ポリシーも適用されます。`metadata["duduclaw:canDecide"]` は表示用情報であり、権限を付与しません。決定ごとに再度認可します。

元のターンを期限付きで保持し、受理した resume により待機中の応答処理を続行します。既存 broker は承認 ID、期限、発生元の関連を永続保存します。runtime の継続状態はメモリ内にあり、gateway の再起動後は復元できず、古い resume はツールを再実行せずエラーになります。再起動後、未完了ターンの thread は元の 15 分の期限まで新しい作業を拒否します。期限後の次の新しい run は、旧ターンの未決定承認を撤回してから受け付けます。承認しただけではツールの実行やプロバイダーによる操作の受理を証明しません。

実行中ターンも一致する完了受領確認もない resume は、受付記録の作成前に拒否します。アカウント別の容量・頻度制限は再起動後も保持し、スタッフ、thread、受領確認リクエスト全体で集計します。超過は HTTP 429 です。SQLite に受付記録を保存し、同じ thread で 24 時間以内に `runId` を再使用すると、本文が同じでも HTTP 409 を返します。resume の再試行には新しい run ID を使ってください。同じ gateway プロセスで最大 15 分間、完了済み resume と同じ決定を送ると、`RUN_STARTED` と success の受領確認を持つ `RUN_FINISHED` を返せます。これは決定の記録を確認するもので、テキストの再送やツールの再実行は行わず、元の作業の成功を証明しません。受領確認のキャッシュは最大 128 件で、再起動後は復元しません。

## 制限

| 項目 | 制限または動作 |
|---|---|
| リクエスト本文 | 256 KiB |
| 入力履歴 | 最大 256 件、ID は制御文字を含まない最大 128 文字 |
| 同時実行 | 呼び出し元ごとに 2 件、gateway 全体で 10 件。保持中の中断ターンも含む |
| 同時ストリーム | 別途、呼び出し元ごとに 2 本、全体で 10 本。エラーと受領確認の SSE も含む |
| 受付記録 | run と thread の合計で最大 4,096 行、アカウントごとに最大 256 行。run ID は 24 時間保存し、同じ thread での再使用は HTTP 409 |
| 受付頻度 | アカウントごとに直近 60 秒で最大 32 回。容量・頻度の超過は HTTP 429 |
| 保持期間 | 承認待ちを含め、元のターン全体で 15 分 |
| SSE keep-alive | 15 秒ごと |
| 遅い受信側 | イベントバッファを制限し、送信の待ち時間は 15 秒まで。作業を無期限に保持しない |
| 切断 | 実行中の配信をキャンセルし、そのターンの未決定承認を撤回。正常に interrupt を送った場合は resume のために保持 |
| 転送 | JSON HTTP/SSE のみ。Protobuf と `Last-Event-ID` 再送は未対応 |
| 入出力 | テキスト入力と最終テキスト応答。添付アップロードとファイル配信 API は未対応 |

入力検査、ツール権限、承認、予算、出力保護には既存の応答処理を使います。[セキュリティ防御](05-security-defense.md)も参照してください。仕様：[AG-UI 1.0 HTTP/SSE](https://docs.ag-ui.com/spec/1.0/basic/transports/http-sse.md)、[run input](https://docs.ag-ui.com/spec/1.0/basic/run-input.md)、[interrupts](https://docs.ag-ui.com/concepts/interrupts)。
