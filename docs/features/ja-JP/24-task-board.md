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
| `tasks_update` | フィールド編集（タイトル／説明／優先度／タグ） |
| `tasks_claim` | 未割り当てカードをアトミックに引き受け`in_progress`にする |
| `tasks_complete` | カードを`done`にし、任意の完了サマリを付与 |
| `tasks_block` | カードを`blocked`にし、理由を必須で付与 |
| `activity_post` | タスクの状態を*変えず*に進捗メモを投稿 |
| `activity_list` | 最近のアクティビティを読む（既定は呼び出し元） |

これがMulticaの「エージェント＝チームメイト」設計の核心です。エージェントは呼び出す関数ではなく——ボードを見張り、カードを拾い、スタンドアップを投稿する同僚です。

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

カード完了時には`completed_at`が自動で押されます。ブロック時にはカードに表示される`blocked_reason`が記録されます。引き受けは **compare-and-set** です。`tasks_claim`はカードが現在未割り当ての場合のみ成功するため、二つのエージェントが同じカードを掴むことはできません。

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

引き受けはcompare-and-setなので、二つのエージェントが二重に引き受けることはありません。親リンクは循環チェックされます。ストアはbusy timeout付きのWALモードで動作するため、ダッシュボードと複数のエージェントがボードを壊さずに並行して書き込めます。

---

## まとめ

チームには、誰もが見て、引き受け、報告できる一枚のボードが必要です。DuDuClawのTask Boardがそのボードです——単一のSQLiteストアに、人間の扉（dashboard RPC）とエージェントの扉（MCPツール）、スタンドアップ用のリアルタイムActivity Feed、稼働中のエージェントが常に自分のキューを見られるプロンプト自動注入、そして作業が自分の名前に届いたときにアイドルなエージェントを起こすスケジューラレベルのプルを備えています。これこそが、AIエージェントを「呼び出す関数」から「カードを拾うチームメイト」へと変えるものです。
