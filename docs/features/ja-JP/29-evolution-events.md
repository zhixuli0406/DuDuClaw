# Evolution Events システム

> エージェントのフライトデータレコーダー——意味のある進化・ガバナンス・耐久性イベントを、確実な配信を保証するバッチ機構ですべて記録。

---

## たとえ話：フライトデータレコーダー

航空機のブラックボックスは飛行機を操縦しません。機体尾部に静かに収まり、意味のあるあらゆるイベント——高度の変化、エンジン状態、操縦入力、警報——を、墜落しても生き残る一度きり書き込みの媒体に記録します。パイロットが飛行中にそれを読むことはありません。しかし何か起きたとき、このレコーダーこそが、実際に何が、どの順番で、なぜ起きたのかを語る唯一の真実の源です。

Evolution Events システムは、DuDuClaw がエージェントのために用意したフライトデータレコーダーです。それ自体は進化を*駆動*しません——進化を駆動するのは GVU ループ、予測エンジン、ガバナンス層、耐久性フレームワークです。レコーダーはただ静かに、それらのサブシステムが発する意味のあるイベントを記録します：スキルが有効化された、セキュリティスキャンが実行された、ポリシーが違反された、サーキットブレーカーがトリップした、リトライが尽きた。各レコードは追記専用（append-only）の JSONL ログに、固定の 8 フィールドスキーマで書き込まれ、発信元のエージェントを決してブロックしません。

数週間後にオペレーターがダッシュボードの **Reliability**（信頼性）ページを開き、「なぜこのエージェントの成功率が下がったのか？」と問うとき、その答えはレコーダーから得られます——記憶でも当て推量でもなく。

---

## 1 レコードのかたち

すべてのイベントは——どのサブシステムが発したものであっても——同じフィールドを持つ 1 行の JSONL にシリアライズされます。固定幅のスキーマ（欠けたキーはなく、欠損値は `null` にシリアライズ）が、下流のパーサーをシンプルに保ちます。

```
AuditEvent {
  timestamp:      "2026-06-21T07:14:02Z"     ← RFC3339、UTC
  event_type:     "gvu_generation"           ← どのサブシステム / アクション
  agent_id:       "duduclaw-pm"              ← 誰に起きたか
  skill_id:       "python-patterns" | null   ← 関与したスキル（あれば）
  generation:     3 | null                    ← GVU 世代カウンター
  outcome:        "success"                   ← success / failure / suppressed / ...
  trigger_signal: "prediction_error" | null   ← 上流の原因
  metadata:       { ... }                     ← 小さな（<1 KB）構造化診断データ
}
```

このスキーマは**追記専用かつ後方互換**です：P0 のバリアントは決して改名・削除されず、後のすべての拡張は純粋に追加のみです。最初のリリース向けに書かれたパーサーが、今でも最新のログを読めます。

---

## イベントカテゴリ

`AuditEventType` 列挙（`schema.rs` 内）は 3 つのドメインにまたがります。当初の P0 集合は進化をカバーし、W19-P1 拡張でガバナンス（Governance）と耐久性（Durability）が加わりました。

| ドメイン | イベント型 | 何を記録するか |
|----------|------------|----------------|
| **Evolution（P0）** | `skill_activate`、`skill_deactivate`、`security_scan`、`gvu_generation`、`signal_suppressed`、`skill_graduate` | スキルのオン/オフ、セキュリティ審査、GVU 自己対戦サイクル、停滞抑制、エージェント間の昇格 |
| **Governance（W19-P1）** | `governance_violation`、`governance_approval_requested`、`governance_approval_decided`、`governance_policy_changed`、`governance_quota_reset` | ポリシー違反、承認ワークフロー、ポリシー CRUD、日次クォータのリセット |
| **Durability（W19-P1）** | `durability_retry_attempt`、`durability_retry_exhausted`、`durability_circuit_opened`、`durability_circuit_recovered`、`durability_checkpoint_saved`、`durability_dlq_replayed` | リトライの試行と枯渇、サーキットブレーカーの状態遷移、チェックポイント保存、DLQ リプレイ |

`Outcome` 列挙も同様に階層化されています：P0 は `success` / `failure` / `suppressed`、W19-P1 で `blocked`、`warned`、`throttled`、`pending`、`approved`、`rejected`、`triggered`、`recovered` が加わります——したがって `governance_violation` は `blocked` になり得て、承認は `pending` になり得て、`durability_circuit_opened` は `triggered` になり得ます。

---

## 4 つのモジュール

システムは `gateway/evolution_events/` 配下の、責務が明確な 4 つのモジュールから構成されます。

| モジュール | 役割 |
|------------|------|
| `schema.rs` | `AuditEvent`（8 フィールドのレコード）、`AuditEventType`（3 ドメインにまたがる 17 バリアント）、`Outcome` を定義。ワイヤフォーマットの唯一の真実の源。 |
| `emitter.rs` | ノンブロッキングの玄関口。`EvolutionEventEmitter` は型付きヘルパー（`emit_skill_activate`、`emit_gvu_generation` …）を公開し、すべての emit は分離した Tokio タスクを spawn するため、呼び出し側が I/O でブロックされることは決してありません。emitter を呼び出し連鎖に通せない箇所のために、プロセスグローバルのシングルトンを提供します。 |
| `query.rs` | 読み取りパス。`AuditEventIndex` は JSONL ファイルの上に構築された SQLite バックエンドのインデックスキャッシュで、`AuditQueryFilter` / `AuditQueryResult` がページネーションとフィルタ付きの読み取りをサポートします。 |
| `reliability.rs` | 分析層。純粋関数が生イベントを時間ウィンドウごとに集計し、エージェントごとの `ReliabilitySummary` にまとめます。 |

（5 つ目のファイル `logger.rs` は emitter が書き込む JSONL アペンダーです——日付 + 10 MB サイズによるローテーション、書き込みエラー時のリトライ 1 回、永続化前の metadata マスキングを行います。）

---

## 書き込みパス：Emit → バッチ → 保存

レコーダーの第一の使命は、エージェントを決して遅くしないことです。書き込みパスは最初から最後まで非同期です。

```
GVU ループ / ガバナンス / 耐久性サブシステム
         |
         | emitter.emit_gvu_generation(agent, gen, outcome, ...)
         v
EvolutionEventEmitter        ← 即座にリターン
         |
         | tokio::spawn（分離）——呼び出し側は I/O でブロックされない
         v
EvolutionEventLogger.log(event)
         |
         | 機密 metadata をマスキング（例：last_error → [REDACTED]）
         v
events/YYYY-MM-DD.jsonl に 1 行の JSON を追記
         |
         +── 日付が変わった？      → 新しい日付ファイルへローテーション
         +── ファイル ≥ 10 MB？    → YYYY-MM-DD-{seq}.jsonl へローテーション
         |
         v
flush() 時に fsync で永続化を保証
```

書き込みが失敗した場合（ローテーションの不調、一過性の FS エラー）、logger は古いファイルハンドルを無効化し、開き直して、レコードを破棄する前に**1 回だけリトライ**します——監査イベントは一瞬の不具合を乗り越えるべきであり、跡形もなく消えてはなりません。

---

## 読み取りパス：保存 → クエリ → Reliability ページ

JSONL は追記には優れますが、フィルタ付きクエリには遅いです。そこで読み取りパスはログを SQLite にインデックス化してから集計します。

```
events/*.jsonl  ──（バックグラウンド同期）──>  AuditEventIndex (SQLite)
                                              |
                  ┌───────────────────────────┼───────────────────────────┐
                  v                            v                           v
        audit.evolution_query        audit.reliability_summary    /api/reliability/summary
        （フィルタ・ページネーション）  （集計済みメトリクス）        （HTTP エンドポイント）
                  |                            |                           |
                  └────────────────┬──────────┴───────────────────────────┘
                                   v
                          Web ReliabilityPage.tsx
                   consistency · task success · skill adoption · fallback rate
```

`AuditEventIndex` は一度だけ開かれ、`Arc` の背後で共有され、バックグラウンドタスクによって同期され続けます。これにより RPC ハンドラと `/api/reliability/summary` HTTP エンドポイントが、それぞれ JSONL を再スキャンするのではなく、1 本の接続を再利用します。

### クエリの安全性

`query.rs` は悪用に対して堅牢化されています：

- **カラム許可リスト**——フィルタ可能なすべてのカラムは `ALLOWED_FILTER_COLS` に含まれていなければならず、リスト外のカラム名は拒否され、将来の SQL インジェクション経路を塞ぎます。
- **クランプされたページネーション**——`limit` は `[1, MAX_LIMIT]` にクランプされ、`offset` にも上限があるため、巨大な OFFSET が SQLite に無制限の行をスキャンさせることはできません（DoS ガード）。

---

## Reliability Summary（信頼性サマリー）

`reliability.rs` は生のイベントストリームを、オペレーターが読める健全性メトリクスに変換します。すべての比率フィールドは `[0.0, 1.0]` の範囲にあり、ウィンドウ内にイベントがない場合、成功志向のメトリクスは保守的に中立な `1.0` を返し、採用率/フォールバック率は `0.0` を返します。

```
ReliabilitySummary（エージェントごと、デフォルト 7 日ウィンドウ）
├─ consistency_score      = 各 event_type の (success / total) の平均
├─ task_success_rate      = success イベント / 全イベント
├─ skill_adoption_rate    = skill_activate イベント / 全イベント
├─ fallback_trigger_rate  = llm_fallback イベント / 全イベント
├─ total_events           = ウィンドウ内でカウントされた監査行数
└─ generated_at           = 計算時の RFC3339 タイムスタンプ
```

各メトリクスは、集計カウントを受け取る小さな**純粋関数**（`avg_success_rate`、`task_success_rate`、`skill_adoption_rate`、`fallback_trigger_rate`）で計算されます——これにより、フィクスチャで容易にユニットテストでき、いかなる I/O も伴いません。

---

## 信頼性の保証

レコーダーは、「記録する」ことが「記録される対象」を決して害さず、レコードが通常の障害を生き延びるよう設計されています。

```
保証                            メカニズム
─────────────────────────────  ────────────────────────────────────────
呼び出し側を決してブロックしない  emit ごとに分離した tokio::spawn
一過性の書き込み失敗を生き延びる  ハンドル無効化 → 開き直し → 1 回リトライ
ファイル成長の上限              日付ローテーション + 10 MB サイズローテーション
機密を漏らさない                JSONL 書き込み前に metadata をマスキング
flush 時に永続化                flush() による fsync
後方互換なスキーマ              P0 バリアントは改名しない；追加のみ
クエリを武器化できない          カラム許可リスト + クランプした limit/offset
固定幅 JSON                    欠損フィールドは null にシリアライズ
```

---

## なぜ重要か

### 結合なしの可観測性

各サブシステムは 1 行を発して、自分の仕事を続けます。GVU ループはダッシュボードの存在を知らず、ガバナンス層は SQLite の存在を知りません。レコーダーはイベントを*生成する*ことと*消費する*ことを分離するため、新しいサブシステムは emit ヘルパーを 1 つ呼ぶだけで記録を始められます。

### 正直な監査証跡

スキーマが後方互換で、ログが追記専用であるため、履歴をこっそり書き換えることはできません。古いレコードは、書かれた当時の意味とまったく同じ意味を持ち続けます——ガバナンスとセキュリティレビューにとって不可欠です。

### 当て推量ではなく、オペレーターへの答え

Reliability ページは数千の生イベントを、オペレーターが本当に気にする 4 つの数字に変えます：このエージェントは一貫しているか、成功しているか、スキルを採用しているか、クラウドフォールバックを避けているか？ある数字が動いたとき、その裏にあるイベントがそこにあり、掘り下げられます。

### 高負荷でも攻撃下でも安全

ノンブロッキングな emit がホットパスを高速に保ちます。ローテーションがディスク使用量を有界に保ちます。クエリ許可リストと offset クランプが、読み取りパスを DoS やインジェクションの攻撃面に変えさせません。

---

## まとめ

ブラックボックスは飛行機を操縦しません——しかしそれがなければ、すべての事故は謎のままです。Evolution Events システムは、DuDuClaw のエージェントに対してまさにその役割を果たします：ノンブロッキングな emitter、リトライ 1 回の耐久性を備えたローテーション JSONL ログ、SQLite でインデックス化したクエリ層、そしてダッシュボードに表示される信頼性の集計。進化・ガバナンス・耐久性のすべてが同一の固定 8 フィールドスキーマを流れるため、エージェントが時間とともにどう変わったかという物語は、常に記録され、常にクエリでき、常に安全に読めます。
