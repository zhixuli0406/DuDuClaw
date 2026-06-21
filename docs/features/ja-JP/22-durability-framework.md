# 耐久性フレームワーク

> 永続性の5本の柱——不安定なネットワーク、一時停止したサービス、実行途中のクラッシュが、操作を黙って取りこぼすことを決して許さない。

---

## たとえ話：小包の物流ネットワーク

すべての重要な書き込み——wiki の編集、メモリの保存、MCP 呼び出し、外部へのメッセージ——を、配達員に渡す一つの小包だと考えてください。

1. **追跡番号**は同じ小包が二度発送されるのを防ぎます。集配所でスキャンし、すでにシステムにあれば、重複したトラックを出しません。
2. **再配達**は受取人が不在のケースに対応します。配達員は少し待ち、戻り、もっと待ち、また戻る——間隔をずらすので、車両群全体が同じ通りに同じ分に殺到しません。
3. **故障ルートのサーキットブレーカー**は地域全体が到達不能になったときに作動します。冠水した道へトラックを次々送り込む代わりに、配車はそのルートを止め、待ち、*一台*の偵察トラックでテストしてから再開します。
4. **返品倉庫**は、あらゆる配達を試みてもなお本当に届けられない小包を収容します——だから決して失われず、後で人間が確認して再発送できます。
5. **配送マニフェスト**は複数停留所の長距離ルートがどこまで進んだかを記録するので、トラックが途中で故障しても、次のドライバーは最後に完了した停留所から再開します——ゼロからではなく。

DuDuClaw の `duduclaw-durability` crate はまさにこの物流ネットワークです——ネットワークのジッター、サービスの一時停止、プロセスのクラッシュをまたいで操作を信頼できるものに保つ、5本の組み合わせ可能な柱です。

---

## 5本の柱

| 柱 | モジュール | 保証 |
|----|-----------|------|
| **Idempotency（冪等性）** | `idempotency.rs` | 同一操作は dedup ウィンドウ内で最大一回だけ実行——重複は元の結果を返す |
| **Retry（リトライ）** | `retry.rs` | 一時的な失敗を指数バックオフ + ジッターで再試行、per-operation ポリシー |
| **Circuit Breaker（サーキットブレーカー）** | `circuit_breaker.rs` | 失敗している依存をシステムを巻き込む前に隔離（Open）し、回復を探る |
| **Checkpoint（チェックポイント）** | `checkpoint.rs` | 長時間タスクの進捗をスナップショット化し、クラッシュ後はゼロでなく最後のフェーズから再開 |
| **DLQ（デッドレターキュー）** | `dlq.rs` | すべてのリトライを使い果たした操作を隔離し、決して失わず、再生可能 |

5つのインターフェースはすべて `duduclaw_durability::prelude` から re-export され、単一の `DurabilityLayer` に組み立てられます。

---

## 柱 1 — Idempotency：追跡番号

すべての操作は決定論的なキーを取得します：

```
{agent_id}:{operation_type}:{content_hash}
```

content hash は SHA-256 の先頭32 hex 文字（128-bit の衝突空間）です。`check_and_record` は単一の書き込みロック内で原子的な compare-and-set を行い——二つの並行呼び出しが両方「New」と判定する TOCTOU 競合を塞ぎます：

```
check_and_record(key, placeholder)
     |
     v
書き込みロック取得
     |
     +-- key は dedup ウィンドウ内に既存？ ──► CheckResult::Duplicate { original_result, ... }
     |
     +-- key は新規 ──► placeholder を挿入 ──► CheckResult::New
     |                       |
     v                       v
ロック解放          呼び出し側が操作を実行し、record(key, real_result)
```

`IdempotencyConfig` のデフォルト値：`dedup_window_seconds = 3600`、`max_key_length = 256`、`cleanup_interval_seconds = 7200`。インメモリ実装はインターフェースを変えずに Redis / SQLite バックエンドへ差し替え可能です。

---

## 柱 2 — Retry：再配達

`RetryEngine` は `operation_type` ごとに `RetryPolicy` を保持します。遅延は指数的に増加し、その後ジッターで揺らされるので、同時に発生した失敗がリトライストームへ同期しません：

```
delay = initial_delay_ms * multiplier^attempt   （上限 max_delay_ms）
        + [0, delay の 30%) の範囲のランダムジッター
```

```
attempt 0   ├─ 500ms ─────┤ リトライ
attempt 1   ├─ 1000ms ─────────┤ リトライ
attempt 2   ├─ 2000ms ──────────────────┤ リトライ
            （各バーにさらに最大 +30% のランダムジッターが加わる）
            └─ 枯渇 ──► DLQ へ送る
```

エラーは分類されます：`non_retryable_errors`（例：`PERMISSION_DENIED`、`INVALID_SCHEMA`）が優先され即座に停止；`retryable_errors`（例：`NETWORK_TIMEOUT`、`SERVICE_UNAVAILABLE`、`RATE_LIMITED`）は再試行されます。デフォルトポリシーは `mcp_call`、`memory_write`、`wiki_write`、`message_send`、`external_api` に対して用意されています。`RetryOutcome` は `success`、`attempts`、`final_error`、`sent_to_dlq` を報告します——リトライ不可の失敗は決して DLQ へ行かず、枯渇したリトライは行きます。

```toml
[retry.mcp_call]
max_attempts    = 3
initial_delay_ms = 500
max_delay_ms    = 10000
multiplier      = 2.0
jitter          = true
retryable_errors     = ["NETWORK_TIMEOUT", "SERVICE_UNAVAILABLE", "RATE_LIMITED"]
non_retryable_errors = ["PERMISSION_DENIED", "INVALID_SCHEMA", "NOT_FOUND"]
```

---

## 柱 3 — Circuit Breaker：故障ルートを閉じる

各依存はそれぞれ独立した三状態ブレーカーを持ちます。ブレーカーはリトライが事態を悪化させる前に失敗している依存を隔離します：

```
        failure_rate > threshold
          （min_request_count 以上で）
 CLOSED ───────────────────────────►  OPEN
   ▲                                    │
   │ probe_success_required 回の成功    │ reset_timeout 経過
   │                                    ▼
   └──────────  HALF_OPEN  ◄────────────┘
                   │
                   │ プローブ失敗
                   └──────────────────►  OPEN
```

- **Closed** — すべての要求が通過；直近 `window_size` 件の結果のスライディングウィンドウで失敗率を追跡。
- **Open** — `reset_timeout_seconds` が経過するまで、要求は `CircuitOpen` で拒否されます。
- **HalfOpen** — 限られた数のプローブを通します。`probe_inflight` の計数が並行プローブを `probe_success_required` で上限化するので、複数のプローブが同時に成功しても過剰計数してブレーカーを早すぎるタイミングで閉じることはありません。

繊細な防護：プローブの結果が決して報告されない場合（panic / cancel / future の drop）、`probe_started_at` と `probe_timeout_seconds` がブレーカーに放棄されたスロットを**再武装**させます——さもなければ永遠に Open に貼り付いてしまいます。inflight プローブのない迷子の `after_call` は完全に無視されます。

すべての遷移は監査証跡のために `StateTransition` コールバックを発火します。デフォルトは `memory_service`（閾値 0.5）、`external_mcp_client`（0.3）、`wiki_service`（0.4）に対して用意されています。

---

## 柱 4 — Checkpoint：配送マニフェスト

長時間実行タスクは進捗をスナップショット化し、クラッシュ後は最後のフェーズから再開します：

```
save("task-123", "agent-1", "phase-2", state_json, ttl=3600)
     |
     v
   [クラッシュ / 再起動]
     |
     v
restore("task-123", "agent-1") ──► Some(Checkpoint { phase: "phase-2", state, ... })
     |
     v
   phase-2 から再開（ゼロからではなく）
```

各 `Checkpoint` は `checkpoint_id`、`task_id`、`agent_id`、`phase`、任意の JSON `state`、`created_at` / `expires_at`、そして系譜のための `parent_checkpoint_id` を持ちます——「チェックポイント X から別のアプローチを探る」という会話状態の分岐を可能にします（RFC-26 §4.2）。`CheckpointConfig` のデフォルト値：24h TTL、`max_checkpoints = 10_000`、毎時クリーンアップ。

---

## 柱 5 — DLQ：返品倉庫

リトライが枯渇すると、操作は破棄されず——デッドレターキューに隔離されます：

```
リトライ枯渇
     |
     v
DLQ.enqueue(agent_id, operation_type, original_operation, retry_count, last_error, ttl)
     |
     v
DlqRecord { status: Pending }
     |
     +──► 再生 ──► status: Replayed
     |
     +──► 断念 ──► status: Abandoned
     |
     +──► TTL 経過 ──► 削除
```

各 `DlqRecord` は完全な `original_operation` JSON、agent、操作タイプ、リトライ回数、最後のエラー、タイムスタンプを保持します——人間（または自動化ジョブ）が失敗した作業を理解し再発送するのに十分な文脈です。ステータスは `Pending → Replayed` / `Abandoned` の間を移動します。

---

## 柱はどう組み合わさるか

単一の耐久性書き込みは5本すべてを貫きます：

```
入ってくる操作
     |
     v
1. Idempotency.check_and_record ── 重複？ ──► original_result を返す
     | New
     v
2. CircuitBreaker.before_call ── Open？ ──► 拒否（早期失敗）
     | 許可
     v
3. RetryEngine.execute(op) ── 一時的失敗？ ──► バックオフ + ジッター、リトライ
     |                                                |
     | 成功                                           | 枯渇
     v                                                v
   CircuitBreaker.after_call(true)            5. DLQ.enqueue
   Idempotency.record(real_result)               CircuitBreaker.after_call(false)
   4. Checkpoint.save(next_phase)
```

これがまさに、gateway の LLM fallback パスと durable cron jobs が走るチェーンです——同じ5つの保証が、自動スケジューリングと主要推論のフォールバックの両方を守ります。

---

## なぜ重要か

### 黙って失われない

DLQ は、リトライを使い果たした書き込みが破棄されずに捕捉されることを保証します。チェックポイントと組み合わせれば、タスク途中のクラッシュは破滅的ではなく回復可能になります。

### 重複した副作用がない

リトライまたは再配達されたメッセージは、本来なら二度投稿したり、メモリを二度保存したり、外部 API に二重課金したりしかねません。冪等キー——決定論的で、内容アドレス指定——は「ウィンドウ内で最大一回」を、並行呼び出し下でも硬い保証にします。

### 連鎖的な障害がない

ブレーカーがなければ、死んだ `memory_service` はすべての agent のすべてのリトライを吸収し、gateway 全体が停止するまで続きます。ブレーカーは早期に作動し、`CircuitOpen` で早期失敗し、自身のスケジュールで回復を探ります。

### ジッターがサンダリングハードを防ぐ

多くの agent が同じ瞬間に失敗すると、決定論的バックオフでは全員が同じミリ秒にリトライします。真のランダムジッター（最大30%）が負荷を分散させます。

### 設定可能、マジックナンバーなし

すべての閾値——リトライ回数、遅延、失敗率、タイムアウト、TTL——は検証済みデフォルト値を持つ設定フィールドです。ポリシーは `upsert_policy` / `upsert_config` でホットリロードできます。

---

## まとめ

物流ネットワークは、トラックが決して故障しないとは約束しません——故障したトラックがあなたの小包を決して失わないと約束します。DuDuClaw の耐久性フレームワークは、すべての重要な書き込みに同じ約束をします：追跡されるので重複せず、リトライされるので一度のしゃっくりで死なず、ブレーカーで守られるので死んだ依存がシステムを道連れにせず、チェックポイントされるのでクラッシュが再開でき、DLQ に支えられるので本当に失敗した作業が黙って失われることは決してありません。
