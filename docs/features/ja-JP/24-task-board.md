# Task Board と Activity Feed

> エージェント＝チームメイトのタスク管理——人間とAIエージェントが共に操作し、引き受け、スタンドアップを投稿する一枚の共有Kanbanボード。

---

## たとえ話：共有Kanbanボード

オフィスの壁に貼られた物理的なKanbanボードを想像してください。カードが列を移動します——*To Do → In Progress → Done*——そして*Blocked*レーンが行き詰まったものを受け止めます。人間のプロダクトリードが新しいカードを貼ります。チームメイトが歩み寄り、「To Do」からカードを剥がし、「In Progress」の自分の名前の下に貼り、スタンドアップで何をしたか報告します。

ここで、チームメイトの一部をAIエージェントにしてみましょう。彼らは同じボードを読みます。同じカードを引き受けます。同じスタンドアップ更新を投稿します。唯一の違いは*どうやって*ボードに触れるかです。人間はwebダッシュボードでマウスを使い、エージェントは自身の推論ループの中からツールを呼び出します。

DuDuClawのTask Boardはまさにこれです——SQLiteに支えられた一枚のボードに、そこへ通じる二つの扉があります。一つは人間用（webダッシュボード）、もう一つはエージェント用（MCPツール）。両者は同じカードを動かします。Activity Feedは全員が読むスタンドアップのログです。

---

## 一つのストア、二つのアクセス層

ボードとフィードは単一のSQLiteデータベース（`tasks.db`、WALモード、マルチプロセス安全のための5秒busy_timeout）に存在します。すべての操作が単一の`TaskStore`に集約されます——しかし、そこへ到達する方法は明確に二つあります：

```
   人間                                      AGENT
   (web dashboard)                          (推論ループ内)
        |                                        |
        v                                        v
 ┌──────────────────┐                 ┌────────────────────────┐
 │ Dashboard WS RPC │                 │  Agent-facing MCP tools │
 │  tasks.list      │                 │   tasks_list            │
 │  tasks.create    │                 │   tasks_create          │
 │  tasks.update    │                 │   tasks_update          │
 │  tasks.remove    │                 │   tasks_claim           │
 │  tasks.assign    │                 │   tasks_complete        │
 │  activity.list   │                 │   tasks_block           │
 │                  │                 │   activity_post         │
 │                  │                 │   activity_list         │
 └────────┬─────────┘                 └───────────┬────────────┘
          |                                       |
          +──────────────┐         ┌──────────────+
                         v         v
                    ┌─────────────────────┐
                    │   TaskStore (単一)   │
                    │   tasks.db (SQLite)  │
                    │   - tasks   table    │
                    │   - activity table   │
                    └─────────────────────┘
```

どちらの扉も「主」ではありません。人間がカードを作りエージェントが引き受ける。エージェントがサブタスクを作り人間が再割り当てする。両者が同じ行を書くため、ボードは常に単一の信頼できる情報源です——同期も、ミラーも、ドリフトもありません。

### Dashboard RPC セット（人間）

認証済みダッシュボードWebSocket経由で提供されます。これらがweb UIのTask BoardページとActivity Feedを駆動します。

| RPC | 用途 |
|-----|------|
| `tasks.list` | ボードビュー用にタスクを一覧／フィルタ |
| `tasks.create` | カードを作成；`activity.new`イベントをブロードキャスト |
| `tasks.update` | フィールド編集／カードを列間で移動 |
| `tasks.remove` | カードを削除 |
| `tasks.assign` | カードをエージェントに再割り当て（`tasks.update`の薄いラッパー） |
| `activity.list` | 最近のActivity Feedイベントを読む |

### MCP ツールセット（エージェント）

MCPサーバー経由でAI runtimeに公開されます。これらにより、エージェントは自分のキューを見て、作業を引き受け、進捗を報告し、カードを完了できます——人間を介さずに。

| MCP ツール | 用途 |
|------------|------|
| `tasks_list` | 自分のキューを見る（既定は呼び出し元；`assigned_to='*'`で全件） |
| `tasks_create` | カードを追加；`created_by`は呼び出し元に自動設定 |
| `tasks_update` | フィールド編集（タイトル／説明／優先度／タグ／`assigned_to`／`depends_on`） |
| `tasks_claim` | 未割り当て（または自分に割り当て済み）のカードをアトミックに引き受け`in_progress`にする |
| `tasks_complete` | カードを`done`にし、任意の完了サマリを付与 |
| `tasks_block` | カードを`blocked`にし、理由を必須で付与 |
| `activity_post` | タスクの状態を*変えず*に進捗メモを投稿 |
| `activity_list` | 最近のアクティビティを読む（既定は呼び出し元） |

これがMulticaの「エージェント＝チームメイト」設計の核心です。エージェントは呼び出す関数ではなく——ボードを見張り、カードを拾い、スタンドアップを投稿する同僚です。


### 誰がカードを変更できるか

カードは割り当て先のAI社員のものです。カードを変更・操作するツールは、呼び出し元とその所有者の関係を確認します。オペレーター（どのAI社員にも対応しないMCPキーで、プロセスも社員用に起動されたものではない）はこの確認の対象外です。AI社員は次の表に従います。

| ツール | AI社員がそのまま操作できる場合 | それ以外 |
|--------|------------------------------|---------|
| `tasks_update` | カードの割り当て先・引き受け者・作成者である | 割り当て先との委任関係が必要（同じ部署、`reports_to` の上下、またはホワイトリストのペア。`[delegation] policy` に従う） |
| 他人のカードの `assigned_to` を自分にする `tasks_update` | — | 作成者が誰であっても、常にその関係が必要 |
| `tasks_claim` | 未割り当てのカード、または自分に割り当て済みのカードを引き受ける | 他の社員に割り当てられたカードは関係が必要。確認はアトミックな引き受けの前に行うため、拒否された呼び出し元がリースを持つことはない |
| `tasks_complete`／`tasks_block` | カードの割り当て先または引き受け者である（作成者は含まない） | 割り当て先との関係が必要。割り当ても引き受けもされていないカードは拒否（先に引き受ける） |
| `task_id` 付きの `activity_post` | カードの割り当て先・引き受け者・作成者である | 割り当て先との関係が必要。存在しない `task_id` は拒否 |

割り当ても引き受けもされていないカードには確認する所有者がいないため、作成者でないAI社員は、更新・完了・ブロックの前に `tasks_claim` で引き受ける必要があります。身元が確認できない呼び出し元や、身元がシステム送信者名（`dashboard`、`cron`、`goal-loop-driver`、`heartbeat`、`autopilot`、`webhook`）の呼び出し元は拒否されます。拒否は呼び出し全体を失敗させ、`tool_calls.jsonl` に記録されます。

AI社員が `tasks_update` を呼ぶ場合は、さらに二つの規則があります。

- **ゴールの文面は凍結。** ゴールモードのカードでは `title` と `description` が判定役の読むゴールなので、AI社員は変更できません（監査理由 `goal_contract_frozen`）。`acceptance_criteria` も同様で、こちらはすべてのMCP呼び出し元が変更できません。オペレーターはダッシュボードから編集します。
- **制御用タグはそのまま。** `outcome:` で始まるタグ（受け入れ契約）、`grant:` で始まるタグ（タスク単位の権限付与）、`auto-research` タグ（毎日の自主調査の印）は、カードの受け入れ方やできることを変えます。AI社員はこれらを追加・削除・並べ替えできません（監査理由 `reserved_tag_change`）。他のタグは自由に変更できます。AI社員の `tasks_create` も、`tags` 引数にこれらのタグを含めると拒否されます。`kind = "goal"` でサーバー自身が作る `outcome:` タグは影響を受けません。

---

## タスクのライフサイクル

カードは`status`と`priority`を持ちます。statusは小さく予測可能なフローを移動します：

```
                tasks_create / tasks.create
                          |
                          v
                      ┌────────┐
                      │  todo  │◄──────────────┐
                      └───┬────┘               │
                          │ tasks_claim        │ （tasks.update
                          v                    │  で再オープン）
                   ┌─────────────┐             │
                   │ in_progress │             │
                   └──┬───────┬──┘             │
       tasks_block    │       │  tasks_complete│
                      v       v                │
                  ┌─────────┐ ┌──────┐         │
                  │ blocked │ │ done │─────────┘
                  └────┬────┘ └──────┘
                       │  （ブロック解除 → tasks_update で todo / in_progress へ）
                       └──────────────────────────────►
```

カード完了時には`completed_at`が自動で押されます。ブロック時にはカードに表示される`blocked_reason`が記録されます。引き受けは **compare-and-set** です。`tasks_claim`は他の誰もカードを持っていない場合のみ成功するため、二つのエージェントが同じカードを掴むことはできません。他の社員に割り当てられたカードは、その社員と委任関係のある呼び出し元だけが引き受けられます（「誰がカードを変更できるか」参照）。

### Status 値

| Status | 意味 |
|--------|------|
| `todo` | 作成済み、未着手（作成時の既定） |
| `in_progress` | エージェントまたは人間が作業中 |
| `blocked` | 行き詰まり——`blocked_reason`を持つ |
| `done` | 完了——`completed_at`を押す |

### Priority 値

| Priority | ランク（urgent → low） |
|----------|----------------------|
| `urgent` | 0（最初に表示） |
| `high` | 1 |
| `medium` | 2（既定） |
| `low` | 3 |

タスクは`tags`、任意の`parent_task_id`（サブタスク用、カードが自身の祖先になれないよう循環検出付き）、`created_by`、`assigned_to`も持ちます。

---

## リアルタイム Activity Feed

意味のある状態変更はすべて`activity`行を書き込み、ダッシュボード購読者へライブで送られます。web UIがカードを作成したりエージェントが移動したりすると、gatewayはWebSocket経由で`activity.new`イベントをブロードキャストします——そのためActivity Feedはリフレッシュなしで更新されます。

```
エージェントが tasks_claim を呼ぶ
        |
        v
TaskStore: UPDATE status=in_progress, assigned_to=agent
        |
        +─► append_activity(task_assigned)
        |
        +─► broadcast_event("activity.new", …)  ──►  Dashboard
                                                     （ライブフィード更新）
```

`activity_post`は、エージェントが「まだ作業中、移行は半分まで来た」とカードの状態を*変えずに*言えるように存在します——列の移動ではなく、スタンドアップのコメントです。

---

## 保留中タスクのプロンプト自動注入

ボードはエージェントが*確認できる*だけのものではありません——未完了の作業は自動的にコンテキストへ押し込まれます。gatewayがエージェントのsystem promptを組み立てる際、`## Your Task Queue`セクションを構築します：

```
build_pending_tasks_section(agent_id):
     |
     v
このエージェントの未完了タスクを取得 (in_progress → todo → blocked)
     |
     v
優先度でソート (urgent → low)、最大5件を取る
     |
     v
箇条書き + MCPツールのリマインダーをレンダリング：
  ## Your Task Queue (7 pending)
  1. [urgent] Fix Discord reconnect loop [in progress]
  2. [high]   Draft Q3 release notes
  3. [medium] Review marketplace skill PR — blocked: needs API key
  +2 more — call tasks_list to see all

  Use `tasks_list`, `tasks_claim`, `tasks_update`,
  `tasks_complete`, `tasks_block` to manage these,
  and `activity_post` to report progress.
```

エージェントに未完了タスクがなければ、プロンプトを引き締めるためこのセクションは完全に省略されます。ストアはgatewayが保持する単一の共有SQLite接続を通じて読み取られ（大量のチャネル返信でのWAL書き込みロック競合を回避）、注入がまだ実行されていない場合のみ呼び出しごとのオープンにフォールバックします。

---

## スケジューラレベルのプル：アイドルなエージェントを起こす

自動注入は*すでに*メッセージに応答中のエージェントをカバーします。しかし、本番のほとんどのエージェントは`heartbeat.enabled = false`で、チャネルメッセージが届くまでアイドル状態です——そのため、割り当てられたカードは決して拾われません。

`HeartbeatScheduler`はこれを**スケジューラレベル**で解決します。30秒ごとのtickで、*registry全体*（heartbeat有効なエージェントだけでなく）をスキャンし、各エージェントに対して`poll_assigned_tasks`を実行します：

```
30秒ごとのスケジューラ tick：
     |
     v
registry内の各エージェントについて（heartbeat.enabled に関係なく）：
     |
     v
poll_assigned_tasks(agent)：
   - このエージェントに割り当てられた最高優先度の `todo` は？
   - 30分以上停滞した `in_progress` タスクは？(updated_at が古い)
     |
     v
起床メッセージを投入 (message_queue.db) し、エージェントに
その todo を tasks_claim するか、停滞タスクに activity_post で進捗を促す
     |
     v
クールダウンゲート：同じ通知を < 1時間以内に送っていればスキップ
   （既存キュー行の LIKE マーカーで判定——追加スキーマ不要）
```

これがなければ、Multicaの「エージェント＝チームメイト」設計は「エージェントはチャネルメッセージが届いたときだけ動く」に退化します。1時間の`LIKE`マーカークールダウンが、30秒tickが同じエージェントへ重複通知を殺到させるのを防ぎます。

---

## なぜ重要か

### 真の共有ワークスペース

人間とエージェントは、同期ジョブでつなぎ合わせた別々のシステムではありません。両者は同じSQLite行を書きます。ダッシュボードで作成されたカードは、エージェントがMCP経由で引き受ける*同じオブジェクト*です——ボードは一枚だけです。

### チームメイトのように振る舞うエージェント

`tasks_claim`／`tasks_complete`／`tasks_block`／`activity_post`は、人間の同僚と同じ動詞をエージェントに与えます——作業を引き受け、完了し、ブロッカーを掲げ、スタンドアップを投稿する。ボードは単なるログテーブルではなく協調のための面になります。

### 失われない作業

自動注入は未完了タスクを毎ターン、稼働中のエージェントの前に出します。スケジューラレベルのプルは、さもなければ自分のキューを決して見ないアイドルなエージェントを起こします。両者が合わさり、かつてタスクがルーティングされず手付かずだった隙間を埋めます。

### 安全な並行性

引き受けはcompare-and-setなので、二つのエージェントが二重に引き受けることはありません。他の社員のカードを変更・引き受けるには、その社員との委任関係が必要です。親リンクは循環チェックされます。ストアはbusy timeout付きのWALモードで動作するため、ダッシュボードと複数のエージェントがボードを壊さずに並行して書き込めます。

---

## まとめ

チームには、誰もが見て、引き受け、報告できる一枚のボードが必要です。DuDuClawのTask Boardがそのボードです——単一のSQLiteストアに、人間の扉（dashboard RPC）とエージェントの扉（MCPツール）、スタンドアップ用のリアルタイムActivity Feed、稼働中のエージェントが常に自分のキューを見られるプロンプト自動注入、そして作業が自分の名前に届いたときにアイドルなエージェントを起こすスケジューラレベルのプルを備えています。これこそが、AIエージェントを「呼び出す関数」から「カードを拾うチームメイト」へと変えるものです。

## ゲートウェイのライブイベント

ダッシュボードは `/ws` の通知を受けてタスク、受信トレイ、担当エージェントの状態を再取得します。5分間隔の補助更新は承認件数だけに残ります。タスクとエージェント状態には定期更新がなく、イベント、再接続、手動の再取得を利用します。承認件数の取得に失敗した場合は前の件数を保持し、`?` で古い可能性を示します。再接続、`events_lost`、または `boot_id` の変更後は、既存の list RPC で再取得してください。

イベントは重複する場合があります。RPC の直接通知とデータベース経由の通知は、どちらも再取得のきっかけであり、確定したレコードではありません。各接続は権限フィルターを通過したイベントだけを `seq: 1` から採番します。ハンドシェイクとイベントには互換性を保つ追加フィールド `boot_id` が付きます。受信が遅れた接続には `events_lost` を送ります。リプレイバッファーや新しい snapshot RPC はありません。従業員プロセスのタスク・承認変更は `events.db` から約2秒間隔で転送し、ゲートウェイ内の実行・目標イベントは直接送ります。

| イベント | 閲覧範囲 |
|---|---|
| `agent.turn_started`、`agent.turn_finished`、`agent.status_changed` | 対象エージェントへの閲覧権限。実行通知には runtime と source を含む |
| `task.created`、`task.updated`、`task.needs_human`、`goal.round_started`、`goal.round_finished`、`goal.done`、`goal.failed` | エージェント権限とタスク audience。制限されたカードは引き続き秘匿 |
| `approval.created`、`approval.decided`、`approval.expired` | その接続の `approvals.get` と同じ判定。現在の Manager であり、パスワード変更が必要なら拒否。`knowledge_add_source` は Admin、またはそのコレクションの現在の編集者／所有者（コレクションを読めないときは Admin 以外を拒否。従業員 Viewer は不要）。operator CLI、ワークフロー有効化、`agent_file_change`、`support_pilot_review`、ボード種類（`strategic_plan`、`agent_hire`）は現在の Admin のみ。`knowledge_quarantine` と `expert_skills_enable` は従業員 Viewer。その後もワークフロー有効化スナップショットと未バインドタスクの audience を確認する |
| `budget.breaker_open`、`autopilot.breaker_open` | Admin |

新しい通知は識別子と列挙状態だけを含み、プロンプト、ツール引数、ファイル内容は送りません。目標イベントは別名を使い、`task.updated` を監視する autopilot ルールを起動しません。source は `channel`、`goal`、`cron`、`heartbeat`、`responsibility`、`assign`、`autopilot` です。turn／round の finished はキャンセルを含む呼び出しの終了を表し、タスクの検収成功を意味しません。

`runs.subscribe {"run_id":"…"}` で `run.step` を購読します。指定 run ID のみ対応し、エージェント全体の購読は拒否します。同じパラメーターの `runs.unsubscribe` で解除します。接続ごとに最大16件、購読操作は毎秒20回（同時1件）、配信は毎秒20ステップです。切断時に解除します。キュー投入前と配信前に現在の権限、タスク audience、履歴プライバシーを検査します。未許可のステップはキュー、欠落フラグ、レート枠に影響しません。制限付き履歴にはステップを配信しません。許可済みキューの溢れやレート超過は `events_lost` で通知します。同時の権限検査は接続ごとに4件までで、満杯時の候補は通知せず破棄します。`runs.get` を再取得して補完してください。配信順が変わる場合があります。

指定 run 購読の制限：dispatch の run ID は完了記録時に初めて割り当てられるため、通常クライアントはそのバッチを事前購読できません。履歴は `runs.get` で取得します。従来の cron／memory／skill／channel ブリッジ通知は検証済み `agent_id` または空オブジェクトだけを含み、対象名や subject は配信しません。

実行イベントは `agent_id`、`source`、`runtime` のみで、会話／タスク ID を含みません。DB ブリッジは ID を検証して保存済み情報を再読込し、8 KiB を超える payload を解析前に拒否します。各ポーリングでイベント種別と対象 ID ごとにまとめ、保存済み情報の検証に成功した対象だけを64件の配信枠に数えます。不正・未登録 ID は枠を消費しません。解析と照会は別途256行までに制限します。サイズ、作業量、配信数の上限による破棄時はカーソルを進め、空の `events_lost` をその回に一度送ります。タスク、エージェント状態、受信トレイはこの通知で再取得します。Web のイベント再取得間隔は最低2秒で、先頭と末尾を反映し、失敗時は最大60秒まで指数バックオフします。

現在、このライブステップ通知を生成するのは Claude だけです。チャネルのツール開始は記録時に通知し、スケジュール／ディスパッチのステップは実行全体の記録後に phase `finished` でまとめて通知します。他の runtime は既存の履歴証拠を保持できても `run.step` は生成しません。要約は文字単位で切り詰めた固定ラベルで、引数のプレビューは含みません。

`approvals.list` と `approvals.get {"id":"…"}` のカードには `digest` が付きます。決定資料を閲覧できない場合は `null` です。get は `{approvals, count}` 形式を使います。`approvals.decide` の任意項目 `expected_digest` に渡すと、不一致時は決定せず再取得を求めます。省略時の動作は従来どおりです。digest はオブジェクトのキーを再帰的に整列した compact UTF-8 JSON の SHA-256（小文字16進数）で、配列順序は保持します。対象は id、agent、kind、summary、可視 payload または bound hash、request kind、status、binding、answer、invalidated reason、作成・期限・TTL、simulation、dashboard-only フラグ、表示された workflow activation（pricing を含む）です。チャネルリンク、閲覧者の権限フラグ、digest 自体は除外します。承認とイベントは別々に書き込むため、その間のプロセス停止は補助更新で補います。

turn イベントの `runtime` は呼び出し開始時に選択した provider です。後続の failover で変わる可能性があり、最終結果を生成した provider の証明ではありません。
