# Autopilot ルールエンジン

> イベント駆動の自動化——*これ*が起きたら*それ*をする。ルールが暴走したら作動するブレーカーパネル付き。

---

## たとえ話：ブレーカーパネル付きのスマートホーム

スマートホームはシンプルなルールで動きます：*日没後に玄関が開いたら廊下の照明を点ける。* *気温が18°Cを下回ったら暖房を入れる。* これらを監視する必要はありません——イベントの発生に応じて自律的に作動します。

しかし素朴なルールエンジンには故障モードがあります：あるルールがイベントを起こし、そのイベントが同じルールを再び起こす。照明が点き、動作センサーが反応し、また照明が点く、を無限に繰り返す。よく設計された家には**ブレーカーパネル**があります——ある回路が短時間に過大な電流を引いたら、ブレーカーが落ちてそれを切り離し、家の残りの部分は動き続けます。

DuDuClaw の Autopilot ルールエンジンは、あなたの agent 群にとってのそのスマートホームです。ルールはイベント（task が作成された、channel メッセージが届いた、ある agent がアイドルになった）を監視し、条件をチェックし、アクション（作業の委譲、通知の送信、skill の実行）をディスパッチします。そして各ルールは独自の三状態サーキットブレーカーを持つため、自己強化ループが発生しても*そのルール*だけが落ち、エンジン全体は止まりません。

---

## 仕組み

エンジンは `tokio::broadcast` イベントバスの上に構築されています。プロデューサー（WebSocket handler、cron スケジューラ、channel リスナー）が `AutopilotEvent` をバスに発行し、`AutopilotEngine` が唯一のコンシューマーとして各イベントに対しすべてのルールを評価します：

```
イベントプロデューサー            イベントバス          ルール評価             アクションディスパッチ
─────────────────                ──────────            ───────────────       ───────────────
WebSocket handler ──┐
Cron スケジューラ  ─┼──> broadcast::channel(8192) ──>  AutopilotEngine ──> ┌─ delegate
Channel リスナー   ─┤         (AutopilotEvent)          各ルールについて： ├─ notify
MCP bridge         ─┘                                   1. イベント一致？   └─ run_skill
                                                        2. 条件通過？
                                                        3. ブレーカー閉？
                                                        4. → アクション実行
                                                        5. → history 追記
```

バスの容量は 8,192——イベントのバーストを吸収でき、遅いデータベース書き込みがプロデューサーにバックプレッシャーをかけません。コンシューマーがイベントを取りこぼすほど遅延した場合、エンジンは遅延カウントをログに記録し、運用者が遅い DB を調査したり容量を上げたりできるようにします。

---

## 5つのイベント型

エンジンは5種類の `AutopilotEvent` を購読します。各イベントは payload を運び、条件が照合できるフィールドマップに平坦化されます：

| イベント | `event_name` | 発火タイミング | 主なフィールド |
|----------|--------------|----------------|----------------|
| **TaskCreated** | `task_created` | Task Board に新しい task が現れる | task オブジェクト（id、title、priority……） |
| **TaskStatusChanged** | `task_status_changed` | task がステータス間を移動 | `task_id`、`from`、`to`、task オブジェクト |
| **ChannelMessage** | `channel_message` | channel にメッセージが届く | `channel`、`agent_id`、`text` |
| **AgentIdle** | `agent_idle` | ある agent がアイドルになる | `agent_id`、`idle_minutes` |
| **CronTick** | `cron_tick` | スケジューラが周期的 tick を発する | `now` |

ルールは関心のある `trigger_event` を宣言するため、`channel_message` ルールが `cron_tick` を目にすることはありません。

---

## 条件

ルールの条件は小さな JSON ツリーです。最上層は `all`（すべての子が通過必須）または `any`（少なくとも1つの子が通過）グループにでき、各葉は演算子で単一フィールドと期待値を比較します：

```
{ "all": [ <condition>, <condition>, ... ] }   ← すべての子が真であること
{ "any": [ <condition>, <condition>, ... ] }   ← 少なくとも1つの子が真
```

葉の条件はパスでフィールドを探し、演算子を適用します：

| 演算子 | 意味 |
|--------|------|
| `eq` | フィールドが期待値と等しい |
| `neq` | フィールドが期待値と等しくない |
| `in` | フィールドが値の配列のいずれかである |
| `gt` / `gte` | フィールドが数値的に大きい（または等しい） |
| `lt` / `lte` | フィールドが数値的に小さい（または等しい） |
| `contains` | 文字列が部分文字列を含む、または配列が値を含む |

**存在しない**フィールドはいかなる比較も満たしません——`eq null` を含めて。これは意図的です：存在しないフィールドが `eq null` に一致することを許したために、あるルールがすべてのイベントに対して大量発火したことがありました。欠落は不一致、例外なし。

---

## 3つのアクション型

ルールのイベントが一致し、条件が通過し、ブレーカーが閉じているとき、エンジンは3つのアクションのいずれかをディスパッチします：

| アクション | 作用 | 必須フィールド |
|------------|------|----------------|
| **delegate** | 対象 agent に bus task を投入 | `target_agent`、`prompt` |
| **notify** | channel にメッセージを送信 | `channel`、`chat_id`、`text` |
| **run_skill** | 対象 agent として skill を実行 | `target_agent`、`skill_name` |

`run_skill` が最も慎重を要します。agent も skill 名もルール設定に由来するからです。エンジンは両方を検証します：

```
run_skill アクション：
     |
     v
target_agent は英数字（allowlist）であること ─── 違反なら拒否
     |
     v
skill_name は安全なファイル stem（allowlist）であること ── "../passwd"、"skill/subdir" を拒否
     |
     v
canonicalize(skills_dir/skill_name)
     |
     v
正規化後パスは canonicalize(skills_dir) で start_with すること ── 逸脱なら拒否
     |
     v
実行
```

パス包含チェック（`canonicalize()` + `starts_with`）により、たとえ文字集合 allowlist をすり抜けた skill 名であっても、agent の skills ディレクトリ外のファイルを参照することはできません。

---

## サーキットブレーカー

各ルールは独自の三状態サーキットブレーカーを持ちます。これは自己強化ループ——あるルールのアクションがイベントを生み、それが同じルールを再び起こす（`task_created → delegate → agent が task を作成 → task_created → ...`）——に対するエンジンの防御線です。

```
            ┌──────────────────────────────────────────────┐
            │                                              │
            v                                              │
      ┌──────────┐   60秒以内に >= 10 回発火    ┌────────┐  │
      │  CLOSED  │ ─────────────────────────>  │  OPEN  │  │
      │ (通常)   │                             │(遮断)   │  │
      └──────────┘ <───────────────────────┐  └────────┘  │
            ^      静かなプローブ窓          │       │      │
            │      (再トリップなし)          │       │ 60秒クールダウン後
            │                               │       v      │
            │                          ┌──────────────┐    │
            └──────────────────────────│   HALF-OPEN  │    │
                                       │ (1回プローブ)│────┘
                                       └──────────────┘
                                          プローブ窓内に
                                          再発火 → OPEN
```

- **Closed** — 通常運用。発火は60秒のスライディング窓でカウントされます。窓内10回の発火でブレーカーが **Open** にトリップします。
- **Open** — このルールのすべての発火が60秒のクールダウン中ブロックされます。エンジンの残りは通常どおり動き続けます。
- **HalfOpen** — クールダウン後、1回のプローブ発火が許されます。プローブ窓内にもう1回発火があれば（ループがまだ活きている兆候）、ブレーカーは Open に再トリップします。静かなプローブ窓なら Closed に戻ります。

状態遷移は `autopilot_history` に記録され Activity Feed に表示されるため、運用者はどのルールがいつ、なぜスロットルされたかを正確に確認できます。

---

## ルール定義

ルールは名前、enabled フラグ、`trigger_event`、`conditions` ツリー、`action` とともに SQLite に永続化されます。以下は緊急の新規 task をオンコール agent に委譲するルールです：

```json
{
  "name": "urgent task → on-call",
  "enabled": true,
  "trigger_event": "task_created",
  "conditions": {
    "all": [
      { "field": "task.priority", "op": "eq", "value": "urgent" },
      { "field": "task.title", "op": "contains", "value": "incident" }
    ]
  },
  "action": {
    "kind": "delegate",
    "target_agent": "oncall",
    "prompt": "An urgent incident task was just created. Triage it."
  }
}
```

agent が長くアイドルしたときに channel へ通知する `notify` ルール：

```json
{
  "name": "idle agent alert",
  "enabled": true,
  "trigger_event": "agent_idle",
  "conditions": {
    "all": [ { "field": "idle_minutes", "op": "gt", "value": 30 } ]
  },
  "action": {
    "kind": "notify",
    "channel": "telegram",
    "chat_id": "12345",
    "text": "An agent has been idle for over 30 minutes."
  }
}
```

---

## ルール CRUD：Dashboard RPC + MCP

ルールは2つのサーフェスから管理され、どちらも fail-closed（Admin scope が必要）です：

```
Dashboard (web UI)                         Agent (MCP)
──────────────────                         ───────────
autopilot.list    ── 全ルールを一覧        autopilot_list ── ルールセットの
autopilot.create  ── ルールを追加                          読み取り専用ビュー
autopilot.update  ── ルールを編集
autopilot.remove  ── ルールを削除
autopilot.history ── 実行ログ
```

すべての `create` / `update` は**書き込み時に** `trigger_event` と `action` 構造を検証します——不正なルールは後で `autopilot_history` で静かに失敗するのではなく、即座に拒否されます。すべての実行（成功、エラー、またはブレーカー遷移）はステータスとエラー文脈を持つ行を追記します。

---

## MCP → events.db ブリッジ

エンジンのプロセス内プロデューサーは broadcast バスに直接 `send()` できます。しかし*別プロセス*——MCP server、Python adapter——から発生するイベントはメモリ内 channel に到達できません。ブリッジは `<home>/events.db` にある SQLite イベントバスです：

```
MCP server (別プロセス)                Gateway プロセス
─────────────────────────────         ────────────────
emit "task.created" ──> events.db ──> バックグラウンドリーダー
                        ┌──────────┐   fetch_since(last_id)
                        │ id (PK,  │        |
                        │  AUTOINC)│        v
                        │ ts       │   AutopilotEvent として
                        │ type     │   broadcast バスに
                        │ payload  │   再発行
                        └──────────┘
```

この SQLite バスはレガシーの `events.jsonl` ファイルバスを置き換えました。安全性を支える列と保証：

- **WAL モード + busy_timeout** — 複数プロセスが行を破損させずに並行書き込みできます（`events.jsonl` のローテーション競合なし、半行ハザードなし）。
- **単調増加 `id INTEGER PRIMARY KEY AUTOINCREMENT`** — リーダーは `fetch_since(last_id)` をポーリングするだけ。カーソルファイル不要、再読み込み不要。
- **組み込みの保持期間** — バックグラウンド prune が7日のカットオフより古い行を削除するため、テーブルが無制限に増えることはありません。

---

## なぜ重要か

### 応答するだけでなく反応する Agent

autopilot がなければ、agent は人間がメッセージを送ったときにしか動きません。ルールがあれば、群は自身の運用イベントに反応します——新しい task が適切な専門家に自動ルーティングされ、アイドルの agent が促され、インシデントが通知をトリガーする——すべて人間を介さずに。

### ループはエンジンを止められない

あらゆるイベント駆動システムで最も危険な故障は自己強化ループです。各ルールのサーキットブレーカーは暴走ルールを60秒のタイムアウトに隔離し、他のすべてのルールは通常どおり動き続けます。悪いルールは「スロットル」に縮退するだけで、決して「エンジン停止」にはなりません。

### 構造的に安全

`run_skill` は agent と skill 名を allowlist で検証し、*さらに*解決後のパスが skills ディレクトリ内に留まることを確認します。`eq null` は存在しないフィールドに一致できません。CRUD は書き込み時に構造を検証します。各ゲートは fail closed です。

### ファイルバスのハザードなしのクロスプロセス

`events.db` ブリッジは、共有 JSONL ファイルのローテーション競合、半行破損、権限の癖なしに、別の MCP プロセスがエンジンへ供給する手段を与えます。WAL が並行性を、単調 id が順序を、prune が増加を処理します。

---

## まとめ

スマートホームはイベントに自動で反応し、ブレーカーパネルは暴走回路が家を焼くのを防ぎます。DuDuClaw の Autopilot ルールエンジンはその両方を agent 群にもたらします：broadcast イベントバス、`all`/`any` 条件を持つ宣言的ルール、検証された3つのアクション型、そしてルールごとの三状態サーキットブレーカー。プロセス内のプロデューサーは直接発行し、プロセス外のプロデューサーは WAL に支えられた `events.db` ブリッジ経由で供給します。無人で動かしても信頼できる自動化——ループが損害を与える前にブレーカーが落ちるからです。
