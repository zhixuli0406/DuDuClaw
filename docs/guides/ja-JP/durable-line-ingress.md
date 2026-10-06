# LINE 受信の永続化と障害復旧

LINE webhook の署名検証に成功すると、ゲートウェイはまずイベントを一括で SQLite に書き込んで commit し、そのあとで HTTP 200 を返します。再送、再起動、別の転送経路で同じイベントを受け取っても、すべてこの受信記録を共有します。イベントの処理は従来どおり、返信、アクセス制御、チャットコマンド、ツールゲートを通ります。

## 最初に確認すること

1. **直接 webhook では LINE Developers Console で webhook の再送（redelivery）を有効にしてください。** 受信箱に書き込めないとき、ゲートウェイは 503 を返し、LINE があとで再送することを前提にしています。再送が無効だと、LINE は 503 を受けても再送せず、そのメッセージは失われます。
2. **relay 経路は確実な受信を保証しません。** `duduclaw-relay` を経由する構成（DuDuClaw OS など）では、relay が LINE のリクエストを受けた時点で LINE に 200 を返し、その後ゲートウェイへ転送します。ゲートウェイ側がディスクフル、受信箱を開けない、停止スイッチがオフ、設定が読めないといった理由で受け取らなかった場合、イベントは消え、LINE も再送しません。ゲートウェイはこれを `relay_frames_total{channel="line",outcome="not_accepted"}`（署名エラーは `bad_signature`）に数え、Activity Feed に `relay_line_rejected` を書きます（同じ種類は 10 分に 1 件まで）。確実な受信が必要な構成では直接 webhook を使ってください。

## 設定

```toml
[channel_ingress]
line_enabled = true          # 停止スイッチ、既定 true
line_late_reply = "push"     # "push"（既定）または "fail"
line_workers = 8             # 通常ワーカー数、1～64、既定 8。ゲートウェイ再起動後に反映
retention_days = 90          # 終了したイベントの保持日数、最小 1
stuck_alert_minutes = 15     # 会話がこの時間止まったら警告、0 で無効
capacity_alert_mb = 512      # DB＋WAL がこの MB を超えたら警告、0 で無効
```

`line_workers` 以外は次に読み込まれた時点で反映され、再起動は不要です。`config.toml` が読めない、または解析できないときは、ワーカーは処理を止め（`line_enabled = false` と同じ扱い）、webhook は 503 を返します。

### 停止スイッチ `line_enabled`

`false` にすると、新しい webhook は 503、まだ始まっていないイベントは `ready` のまま待ち、すでに実行中のターンは最後まで走りますが、返信や進捗通知を送る前にスイッチを再確認し、オフなら送りません。受信記録、重複排除の記録、人による判断はすべて残り、24 時間の payload 削除も通常どおり動きます。オンに戻すと、まだ有効な待ちイベントから処理が再開します。

このスイッチで **v1.69 以前の受信方式に戻ることはありません**。オフにすると LINE が止まります。確実な受信を有効にした状態と旧版との違い：

| 項目 | v1.69.x 以前 | 現在 |
|------|------|------|
| 200 を返すタイミング | 署名検証直後、処理はバックグラウンド | 一括で SQLite に書き込み commit した後 |
| 途中でプロセスが落ちた | メッセージ消失 | 受け取ったイベントは再起動後に続行。実行済みかもしれないものは `uncertain` |
| 同じ会話の複数メッセージ | 並行して処理 | 受信順に 1 件ずつ |
| 同時に処理する会話数 | 無制限 | `line_workers`（既定 8）＋判断用ワーカー 1 |
| reply token の期限切れ | Push に切り替え | `line_late_reply` に従う。既定は引き続き Push |
| LINE 認証情報未設定時の Verify | 200 | 503 |
| `config.toml` が読めない | そのまま処理 | 503、処理停止 |
| チャットコマンドと添付の帰属 | メインの AI エージェント | そのメッセージのルーティング先の AI エージェント |

### 返信期限切れ `line_late_reply`

LINE Messaging API のリファレンスによると、reply token は 1 回しか使えず、webhook を受け取ってから 1 分以内に使う必要があります。ゲートウェイは「自分の受信時刻」と「イベントの `timestamp`」の早いほうから 60 秒で判定します。待ち行列やターン自体が長いと、この期限を過ぎます。

**再送**された webhook（`deliveryContext.isRedelivery = true`。webhook の再送をオンにしたときだけ届きます）は、すでに期限切れとして扱います。LINE は、元の配信で使用済みの場合やイベント発生から 20 分を過ぎた場合を除き token を使えるとしていますが、ゲートウェイには最初の配信が届いたかどうかが分かりません。そのため token は試さず、`line_late_reply` に従います。Reply API が HTTP 400 でメッセージ `Invalid reply token`（使用済みまたは期限切れ）を返した場合、`"push"` なら確認し直したうえで Push で送り、`"fail"` ならイベントは `undelivered`（理由 `reply_token_invalid`）になります。それ以外の拒否は従来どおり `undelivered`（`reply_rejected`）です。

- `"push"`（既定）：期限を過ぎたら Push API で、**元のイベントと同じ会話**（グループ、トークルーム、1 対 1 のユーザー）にだけ送ります。送信前に通常の返信と同じ確認を行います：処理リースがまだ有効、停止スイッチがオン、アカウント認証情報とルート／権限のスナップショットが変わっていない。リクエストには `X-Line-Retry-Key` を付け、結果（LINE の `x-line-request-id` を含む）を試行の受領記録に `delivered_via` = `push` として残します。Push は公式アカウントのメッセージ枠を消費します。無料・ライトプランで枠を使い切ると LINE は 429 を返し、イベントは `undelivered` になります。
- `"fail"`：最後の返信を Push に切り替えません。ワーカーがイベントを取った時点で期限切れなら **ターンを実行せず**、イベントを `failed_before_dispatch`（理由 `late_reply_expired`、再送された webhook では `redelivered_reply_token_not_used`）にして運用者に通知します。実行中に期限を過ぎた場合は返信を送らず、`undelivered` になります。期限内に始まったターンの進捗通知と承認・判断カードは Push で送るので、Push の使用は減りますがゼロにはなりません。長いターンではユーザーに返信が届きません。

## 受け付けの範囲

イベントには LINE の `destination` と `webhookEventId` が必要です。署名付き envelope 内の全イベントは 1 つの `IMMEDIATE` トランザクションで書き込み、どれか一つでも失敗すれば全体を rollback します。SQLite は WAL、`synchronous=FULL`、5 秒の busy timeout で動きます。200 は一括で永続的に受け付けたという意味で、モデル、ツール、送信の成否はイベントの状態で確認します。

200 を返す前に行うのは、検証と書き込みに必要な確認だけです：`config.toml` を 1 回読んで認証情報と停止スイッチを得る、署名検証、envelope の解析、書き込み。エージェントの設定が読めない、あるエージェントの `agent.toml` が壊れている、チャネル設定値が不正、といったことで webhook が 503 になることはありません。イベントには「どの認証情報で署名を検証したか」を記録し、commit 直後にルート／権限のスナップショットを追加します。**スナップショットが保存されるまで、どのワーカーもそのイベントを取りません。** 読めない場合は 5、10、20、40 秒の間隔で再試行し、5 回失敗するか、スナップショットのないイベントが 5 分を超えた場合（途中でゲートウェイが止まっていた場合など）は、後の設定を黙って採用せず `quarantined`（理由 `snapshot_unavailable`）にします。一度も実行されていないので `retry` でき、再試行時にはその時点の設定でスナップショットを取ります。スナップショット時点で認証情報が変わっていた、ルートが消えていた場合は `account_route_authorization_changed` です。

重複排除キーは `channel + account(destination) + webhookEventId` の長さ区切り SHA-256 です。

初回実行の `run_id` は `ingress_id` と同じです。運用者が明示的に `retry` または `rerun` すると、同じトランザクションで新しい承認 UUID を新しい `run_id` として保存します。元の `ingress_id` は変わりません。古いワーカーは新しい run の受領記録を確定できません。

## ルートと権限のスナップショット

処理開始前、返信送信前、進捗通知のたびに、スナップショットを計算し直してイベントに保存されたものと比較します。対象は「このメッセージを誰が、どの権限で処理するか」だけです：

- ルート：解決されたエージェント、その trigger／role／status、`[channels]`、`allowed_channels`、`default_agent`、このユーザーのエージェントバインディング。
- 権限：LINE 認証情報のダイジェスト。ルーティング先のエージェントの実効設定（職務プリセットに紐づくエージェントは `agent_resolved/<id>.toml`、それ以外は `agent.toml`）のうち身元フィールド、capabilities、permissions、budget、container の sandbox／network。この会話とグローバルの allowlist、blocklist、pairing、admin、mention-only、binding 設定。このユーザーのペアリング状態。

エージェントを追加したり、関係のないエージェントの設定を変えたりしても、スナップショットは変わりません。プロンプト文、SOUL、ハートビート、統計も影響しません。

比較の結果は 2 種類に分けます：

- **読めて、しかも違う**（再バインド、認証情報のローテーション、権限変更、エージェントの削除）：イベントは `quarantined`（理由 `account_route_authorization_changed`）。古いメッセージを新しいアカウントや新しい相手で処理することはありません。
- **読めない、または解析できない**（設定の書き換え中、DB が混雑）：イベントは `ready` に戻り、5、10、20、40 秒の間隔で再試行します。5 回続けて読めなかったときだけ `quarantined`（理由 `revalidation_unavailable`）になります。このイベントは一度も実行されていないので `retry` できます。
- 最後の返信や進捗通知を送る前にスナップショットが読めない場合は、約 7 秒の間に何度か読み直します。それでも読めなければ送らず、イベントは `undelivered`（理由 `revalidation_unavailable`）になります。「権限が変わった」（`authorization_changed_before_delivery`）とは記録しません。こちらは読めて、しかも違った場合だけに使います。

## 状態と人による処理

| 状態 | 意味 | 同じ会話の後続を止めるか | 可能な処理 |
| --- | --- | --- | --- |
| `ready` | 受付済み、待機中（バックオフ中のこともある） | はい | |
| `claimed` | 90 秒のリースを取得、未実行。期限切れで `ready` に戻る | はい | |
| `dispatching` | ターン実行中、20 秒ごとにリース更新 | はい | |
| `completed` | ターン完了、返信または Push を LINE が受け付けた | いいえ | |
| `failed_before_dispatch` | 実行していないことが確定（例：`late_reply_expired`） | いいえ | `close`、`retry` |
| `undelivered` | ターンは実行済み、返信は届いていない（LINE が拒否、送信前に権限が変わった、期限切れで fail 設定） | いいえ | `close`、`rerun` |
| `uncertain` | 実行済みかもしれず、送達の受領記録がない（処理中にプロセスが消えた、送信中に接続が切れた） | はい | `close`、`rerun` |
| `quarantined` | ルート／権限の変更、設定またはスナップショットが繰り返し読めない、payload の期限切れ、バックアップからの復元 | はい | `close`。理由が `revalidation_unavailable` または `snapshot_unavailable` なら `retry`、`restored_from_backup` なら `rerun` も可 |
| `closed` | 運用者が終了済み、または保存期間後にシステムが終了（`retention_closed`） | いいえ | |

受信箱は v1.69 より前のリリースには含まれていません。プレビュー版で作られた DB はアップグレード時に移行されます：`failed` は `undelivered` に変わり（それらはすべて実行済みでした）、古い形式のスナップショットで保存された `ready` のイベントはアップグレード後に `account_route_authorization_changed` として隔離され、終了しかできません。

処理：

- `close`：ここで終了。記録は残し、メッセージ本文と reply token はすぐに削除します。
- `retry`：実行していないことが確定したイベント専用。もう一度処理に回します。
- `rerun`：実行したかもしれない、または実行したイベント向け（`uncertain`、`undelivered`、復元で止めた `quarantined`）。**ユーザーに返信が重複して届く、ツールが二重に動く、タスクが二重に作られる可能性があります。** `confirm_duplicate_risk=true` と理由が必須で、確認、理由、確認した provider の受領番号（なければ省略）を新しい run と一緒に保存します。

reply token の有効期限はイベントが届いた時点から数えられ、再実行しても延びません。そのため：

- `line_late_reply = "push"`：`retry` や `rerun` の返信は必ず Push で元の会話に送ります。
- `line_late_reply = "fail"`：元の返信期限を過ぎたイベントへの `retry` と `rerun` は拒否され、「回覆期限已過，目前設定為不改用 Push，重新執行不會有回覆；要送達請把 line_late_reply 設為 "push"，或直接結案」（返信期限が過ぎており、Push を使わない設定なので再実行しても返信は届かない。届けるには line_late_reply を "push" にするか、そのまま終了する）と表示されます。届かないと分かっているターン（ツール、費用）を丸ごと走らせないためです。判断のあとで設定を fail に変えた場合も、ワーカーは期限切れとして実行しません。

どの処理も元のアカウント／ルート／権限スナップショットの確認を再び通ります。スナップショットが変わっていれば隔離されます。

### 警告

イベントが `uncertain`、`quarantined`、`undelivered`、`failed_before_dispatch`（期限切れ）になったとき、ある会話でいちばん古い待ちメッセージが `stuck_alert_minutes` を超えたとき、DB が `capacity_alert_mb` を超えたとき、復元後にイベントを止めたとき、ゲートウェイは警告を受信箱 DB のキューに記録します。30 秒ごとに、種類と理由ごと、10 分の時間枠ごとにまとめます：

- 種類・理由・時間枠ごとに Activity Feed を 1 件（`channel_ingress_uncertain`、`channel_ingress_quarantined`、`channel_ingress_undelivered`、`channel_ingress_late_reply_failed`、`channel_ingress_stuck`、`channel_ingress_capacity`、`channel_ingress_restored_held`）。件数と最初の 5 件のイベント ID を含みます。同じ時間枠であとから来た警告は、枠が終わったときに 1 件の補足にまとめます。
- メインの AI エージェントの `[proactive]` 通知先を通じて運用者に通知を送ります。同じ種類は 10 分に 1 件までです。

時間枠と通知の記録は受信箱 DB に保存するので、再起動しても同じ行を繰り返さず、通知の制限もリセットされません。内容はイベント ID の先頭 12 文字、状態、理由コードだけで、メッセージ本文、LINE ユーザー ID、token は含みません。警告は下のコマンドラインとダッシュボードの承認待ちを案内します。**ダッシュボードにはまだ受信箱のページがありません**（「既知の制限」を参照）。

### ダッシュボード RPC（Admin）

- `channel_ingress.list`：1 ページ最大 200 件。状態、理由、バックオフ時刻、状態ごとの件数、DB／WAL のバイト数、現在の設定、最近の試行と人による処理を返します。`before_seq` で古いページを取得します。payload と reply token は返しません。
- `channel_ingress.inspect`：1 件のイベントの試行（`provider_receipt`、`delivered_via`、進捗 Push の集計を含む）と再承認の記録（`action`、`confirmed_duplicate_risk` を含む）。判断イベントは保存された request ID で承認記録を読み取り専用で照合します。
- `channel_ingress.resolve`：`ingress_id`（16 進 64 桁）、`expected_revision`、`expected_attempt`、`action`（`close`／`retry`／`rerun`）、`note`、任意の `provider_receipt`、`rerun` では `confirm_duplicate_risk=true` が必須。呼び出しごとに（成功でも拒否でも）セキュリティ監査 `channel_ingress_resolution` を 1 件書きます。

3 つとも毎回ユーザー DB を読み直して Admin であることを確認し、LINE ワーカーと同じ DB 接続を使います。

### コマンドライン

```bash
duduclaw ops channel-ingress list
duduclaw ops channel-ingress show <ingress_id>
duduclaw ops channel-ingress resolve <ingress_id> --note "理由" [--retry] [--provider-receipt <番号>]
duduclaw ops channel-ingress rerun <ingress_id> --note "理由" --confirm-duplicate-risk [--provider-receipt <番号>]
duduclaw ops channel-ingress batch --action close|retry|rerun --status <状態> [--reason <理由コード>] --note "理由" [--confirm-duplicate-risk] [--limit 200]
```

AI エージェントが Bash で `duduclaw`／`duduclaw-pro` の `ops channel-ingress` を実行すると、agent-file-guard hook が止めます（`BlockedOperatorCommand`）。照合の前に、コマンドを bash と同じように読み直します（行継続、語にくっついた引用符、語にくっついたリダイレクト、`env`／`npx` の前置、絶対パス）。これは減速帯にすぎません。変数、エイリアス、スクリプト、名前を変えた実行ファイルで回避でき、Claude 以外の runtime はこの hook を実行しません。本当の関門は下のダッシュボード承認です。

`list` と `show` はそのまま答えます。`resolve` と `rerun` は最初の実行では**反映されず**、承認リクエストを 1 件作り、「請到儀表板的待辦核准」（ダッシュボードの承認待ちへ）と承認番号を表示して非ゼロで終了します。コマンドラインでは、実行したのが運用者なのか Bash を持つ AI エージェントなのか区別できないため、この種の変更は必ず Admin がダッシュボードで承認します。チャネル上の返信では承認できません。承認から 30 分以内に**同じコマンド**（同じ理由と受領番号）をもう一度実行すると反映されます。承認は 1 回しか使えず、その間にイベントの状態が変われば無効になり、申請し直しです。同時に待てるコマンドラインのリクエストは 20 件までです。待っている間にイベントの状態が変わると、待機中のカードは取り下げられ、次の実行で新しいカードが作られます（カードの内容をその場で書き換えることはありません）。申請、反映、拒否のたびにセキュリティ監査 `channel_ingress_cli_action` を 1 件書きます。AI エージェントのセッションの環境変数を持つプロセスは通常拒否されますが、その変数を外せば回避できます。本当の関門はダッシュボード承認です。急ぐときはダッシュボード RPC を使ってください。

`batch` は 1 回の承認でまとめて処理します。1 つの状態（`uncertain`、`quarantined`、`undelivered`、`failed_before_dispatch`）と、任意で 1 つの理由コードに当てはまり、その処理が適用できるイベントを最大 `--limit` 件（既定 200、上限 500）選び、処理・選択条件・理由・各イベントの ID とその時点の状態バージョンに結びついた承認を 1 件作ります。Admin が承認したあと同じコマンドをもう一度実行すると 1 回だけ反映されます。状態が変わっていないイベントは処理し、その間に変わったものは飛ばし、結果に `applied`、`skipped_changed`、`failed` を表示します。判断の前に選択結果が変わった場合は、待機中のカードを取り下げて作り直します。`batch` の `rerun` も単独の場合と同じく `--confirm-duplicate-risk` が必要です。

`list` と `show` は LINE のアカウント ID と会話 ID を短いダイジェスト（`#` と 16 進 12 文字）で表示します。同じ会話のイベントは対応が分かりますが、LINE のユーザー ID やグループ ID は表示しません。

## 運用者が知っておくこと

**順序のルール。** 同じアカウント・同じ会話では、前に `ready`、`claimed`、`dispatching`、`uncertain`、`quarantined` のメッセージが残っている限り、通常メッセージは処理されずに待ちます（バックオフ中の `ready` も含みます）。`completed`、`closed`、`undelivered`、`failed_before_dispatch` は後続を止めないので、あるメッセージが `undelivered` や `failed_before_dispatch` になると、同じ会話の後続メッセージは処理が進みます。`retry`／`rerun` はイベントを元の受信順で `ready` に戻します。同じ会話でまだ始まっていないメッセージより先に実行されますが、すでに処理された後続メッセージはやり直されないため、再実行の返信が後続メッセージへの返信より遅れて届くことがあります。`uncertain` と `quarantined` の間は後続がすべて待つので、この 2 つの状態から `rerun`／`retry` した場合は順序が保たれます。判断メッセージ（承認、拒否、回答）は待たず、他を止めることもありません。会話どうしは互いに影響しません。

**バックアップからの復元。** デバイスのバックアップ（`device.backup_create`、定期バックアップ、`duduclaw export`）はどれも `channel_ingress.db` を含み、元のデバイスがバックアップ後に待機中のメッセージを処理している可能性があります。そのためデバイスの復元（`device.backup_restore`）は**データを動かす前に** 1 回限りのマーカーを書きます。書けなければ復元を中止し、理由を示します（データは入れ替わりません）。次にゲートウェイが受信箱を開くと、どのワーカーも取得する前に、バックアップ時点で `ready`、`claimed`、`failed_before_dispatch` だったイベント、および再試行できる理由（`revalidation_unavailable`、`snapshot_unavailable`）で `quarantined` だったイベントをすべて `quarantined`（理由 `restored_from_backup`）にし、マーカーを削除します。これらはこのマシンでは一度も実行されていませんが元のデバイスで処理済み、または再試行済みかもしれないため、`retry` はできません。終了するか、重複のリスクを確認したうえで `rerun` してください。ゲートウェイは Activity Feed（`channel_ingress_restored_held`）に記録し、止めた件数を運用者に通知します。バックアップ時点で `dispatching` だったものは `uncertain` になり、これも自動では再実行されません。`duduclaw export` のアーカイブを新しいマシンに手で展開した場合はマーカーが残らないので、先に LINE を止め（`line_enabled = false`）、確認してからオンに戻してください。

## 進捗通知とファイル通知

進捗通知（長いターンの間 1 分に 1 件までの Push。グループからのメッセージでも送信者との 1 対 1 チャットに送ります）は付随メッセージで、`line_late_reply` の設定に関係なく送ります。送信のたびに再確認し、結果は試行の受領記録の進捗集計に別に残します。失敗しても成功したターンが `uncertain` になることはなく、会話も止めません。ターン終了時に最大 10 秒だけ結果を待ち、戻らなかったものは unknown として数えます。

📎DELIVER による「ファイルの準備ができました。ダッシュボードからダウンロードしてください」という通知は別に Push せず、このターンの返信の後ろに付け、同じ返信（または期限切れ時の Push）経路、同じ再確認と受領記録で送ります。

## データ保持と容量

元の単一イベントの JSON（reply token を含む）は別の `ingress_payload` テーブルに最大 24 時間保存し、イベントが `completed` になるか `close` されると即座に削除します。待機中のイベントの payload が期限切れになると `quarantined`（`payload_retention_expired`）になり、終了しかできません。`undelivered` と `failed_before_dispatch` のイベントの payload も 24 時間で消えるので、それ以降は `retry` も `rerun` もできず、終了だけになります。

終了したイベント（`completed`、`closed`）は、試行、受領記録、再承認、人による処理の記録とともに `retention_days`（既定 90 日）後に削除します。`undelivered` と `failed_before_dispatch` は何も止めないので、`retention_days` を過ぎるとシステムが終了し（理由 `retention_closed`）、一緒に削除します。LINE の再送は数時間以内に起こるため、90 日後の削除は重複排除に影響しません。会話を止める `uncertain` と `quarantined` は自動では削除しません。DB＋WAL が `capacity_alert_mb` を超えると 1 日 1 回警告します。容量が尽きると送信元は 503 を受け取り、重複排除に必要な記録は削除しません。

DB ファイルの権限は 0600 です。開くときはシンボリックリンクを拒否し、既存の DB ファイルを「開いて閉じる」ことはしません（それをするとこのプロセスがそのファイルに持つ POSIX ロックが外れます）。SQLite の `secure_delete` と WAL checkpoint は稼働中の DB から削除済みの内容を消しますが、バックアップや外部のコピーは対象外です。

## 既知の制限

- **ダッシュボードに受信箱のページはありません。** イベントはコマンドライン（`list`、`show`、`resolve`、`rerun`、`batch`）で確認・処理し、承認はダッシュボードの承認待ちで行うか、Admin RPC を使います。件数が多いときはまとめて少しずつ処理してください。
- `X-Line-Retry-Key` が重複を防ぐのは同じ run の中だけです。`rerun` は新しい run で新しいキーを使うため、`push_delivery_uncertain` のあと LINE は再実行の Push と最初の Push を区別できません。再実行の前に provider の受領記録を確認してください。
- Push 枠を使い切ると（HTTP 429）イベントは `undelivered` になります。返信は保存しないので、あとで送るには `rerun` でターン全体（モデルとツール）をやり直すことになります。
- `line_late_reply` などの設定はファイルの更新時刻とサイズでキャッシュします。時刻の精度が粗いファイルシステムで、同じ時刻の刻みの中で長さを変えずに書き換えると（`"push"` ↔ `"fail"`）、読み込みが遅れることがあります。次の変更か再起動で反映されます。
- 返信が空のとき、イベントは何も送らずに完了します（従来どおり）。

## 検証の範囲

ローカルテストで確認していること：fail 設定で期限切れのイベントへの `retry`／`rerun` が拒否されること、判断後に fail に変えた再実行は実行も送信もしないこと。Bash 経路で `ops channel-ingress` が止まること。復元マーカーがあるとワーカーが取得する前に待機中のイベントを止め、マーカーは 1 回だけ効くこと。署名不正、一括 rollback、ディスクフル、DB 読み取り専用、DB 混雑で 200 を返さないこと。同じイベントの webhook 20 件同時でも実行は 1 回。待ち行列による期限切れの 2 つの設定（push は同じ会話へ送る、fail は実行も送信もしない）。進捗 Push の失敗がイベントの状態に影響しないこと。ファイル通知が返信に含まれること。プリセットの実効設定がスナップショットに入ること。関係のないエージェントの変更や破損がスナップショットと ACK に影響しないこと。設定が読めないときは即時隔離せずバックオフすること。webhook handler とワーカーを通した実際の OS kill（ACK 後、実行中）。コマンドラインの変更にはダッシュボードの Admin 承認が必要で、承認は 1 回だけ使え、状態が変われば無効になること。スナップショットが保存されるまでイベントを取らないこと、読めないスナップショットはバックオフし、停止中にスナップショットが取れなかったイベントは `snapshot_unavailable` で隔離されること。再送された webhook は reply token を試さないこと（push なら Push、fail なら実行しない）。reply token が拒否されたとき push なら Push に切り替え、fail なら `undelivered` のままであること。送信前に読めない場合は `revalidation_unavailable`、読めて違う場合は `authorization_changed_before_delivery` と記録すること。500 件の警告が件数付きの Activity 1 件にまとまり、再起動しても繰り返さないこと。`batch` は状態が変わっていないイベントだけを処理し、選択結果が変わればカードを作り直すこと。コマンドラインの出力に LINE の生の ID が出ないこと。マーカーが書けないとき復元はデータを動かす前に止まること。行継続、引用符の連結、リダイレクトがくっついた書き方も Bash 経路で止まること。

実際の LINE アカウントでは、再送、reply token の実際の期限、使用済み・期限切れの token に LINE が実際に返すエラー内容（ゲートウェイはメッセージ `Invalid reply token` と照合し、それ以外の 400 は `reply_rejected` のまま）、Push 枠を使い切ったときの応答、`X-Line-Retry-Key` による重複排除、relay 上流の挙動をまだ確認していません。ローカルテストは偽の provider を使っています。
