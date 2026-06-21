# ガバナンスレイヤー（Governance Layer）

> 建築基準法とメーター——宣言的なYAMLポリシーがすべてのエージェント動作をゲートし、フェイルセーフで、再起動なしにリロードする。

---

## たとえ話：建築基準法とメーター

現代の建物は2種類のガバナンスで動いています。

**建築基準法**は、あなたが*何をしてよいか*を決めます。バルコニーは増設できますが、耐力壁は取り壊せません。自分で行ってよい変更もあれば、署名入りの許可と検査官を要する変更（主幹線の配線替え）もあります。ルールは明文化され、公開掲示され、特定の例外が認められない限り全員に適用されます。

**メーター**は、あなたが*どれだけ消費してよいか*を決めます。電気メーターはあなたが*何をするか*には関心がなく、月間の枠に対する総使用量だけを追跡します。ソフトしきい値を超えると請求書に警告が出ます。ハード上限を超えると供給が止まり、次の請求サイクルがメーターをリセットするまで戻りません。

DuDuClawのガバナンスレイヤーは、まさにこの2つの面が、すべてのエージェントの前に立つものです。**PermissionとLifecycleポリシー**は建築基準法——エージェントが何をしてよいか、何が承認を要するか。**RateとQuotaポリシー**はメーター——スロットルや遮断の前に、エージェントがどれだけ速く、どれだけ消費できるか。いずれも素のYAMLで宣言され、いずれも編集時にリロードされ、ルールブックが欠落・不正形式の場合はいずれもフェイルクローズします。

---

## 4つのポリシータイプ

`duduclaw-governance` crateは、すべてのガバナンスを単一のタグ付き列挙 `PolicyType` としてモデル化し、4つのバリアントを持ちます：

| ポリシータイプ | ガバナンス対象 | 主要フィールド | デフォルト処置 |
|----------------|----------------|----------------|----------------|
| **Rate**（`RatePolicy`） | 単位時間あたりの操作回数 | `resource`、`limit`、`window_seconds`、`action_on_violation` | `reject` |
| **Permission**（`PermissionPolicy`） | エージェントが使えるscope | `allowed_scopes`、`denied_scopes`、`requires_approval` | denied優先 |
| **Quota**（`QuotaPolicy`） | 日次の消費予算 | `daily_token_budget`、`max_concurrent_tasks`、`max_memory_entries`、`reset_cron` | `00:00` UTCにリセット |
| **Lifecycle**（`LifecyclePolicy`） | エージェントの健全性とアイドル挙動 | `max_idle_hours`、`health_check_interval_seconds`、`auto_suspend_on_violation_count` | 自動サスペンド |

各バリアントは `policy_id`（一意な識別子）と `agent_id` を持ちます——`"*"` はそのポリシーが**すべての**エージェントに適用されることを意味します。各バリアントは `validate()` を実装し、不正なポリシー（例：`limit: 0`）は黙って受理されるのではなく拒否されます。

---

## ポリシー解決：エージェントがグローバルを上書き

`PolicyRegistry` はディレクトリからポリシーを読み込み、エージェントごとに解決します。優先順位は厳格かつ一方向です：

```
解決順序（優先度の高い順）
─────────────────────────────────────────
  policies/{agent_id}.yaml      ← エージェント固有の上書き
        ↓ 上書き
  policies/global.yaml          ← グローバルデフォルト（"*"）
        ↓ 上書き
  システムコード内蔵デフォルト
```

`get_policies_for_agent("alice")` を呼ぶと、registryは：

```
1. "alice" のエージェント固有ポリシーを収集
2. グローバルポリシーを収集（agent_id = "*"）
3. 各グローバルポリシーについて：
     同じ (policy_id, type_name) を持つ
     エージェントポリシーは存在するか？
        ├─ はい → グローバルをスキップ（エージェント版が勝つ）
        └─ いいえ → グローバルを継承
4. マージ済みリストを返す
```

重複排除キーは **(policy_id, type_name)** であり、`policy_id` 単独ではありません。これが重要です：あるエージェントの `shared` という名の `permission` ポリシーは、たまたま同じく `shared` という名のグローバル `quota` ポリシーを消し去ってはなりません。同じid*かつ*同じtypeのエージェントポリシーだけがグローバル対応版を上書きし、それ以外は両方とも生き残ります。

---

## フェイルセーフな読み込み：不正をスキップ、有効を保持

ポリシーが1つ不正形式なだけでクラッシュするガバナンスシステムは、無いより悪い——ゲート全体を落としてしまうからです。registryは**フェイルセーフ**に読み込みます：不正なポリシーは警告とともにスキップされ、他のすべての有効なポリシーは動き続けます。

```
load() が policies/ を走査
     |
     v
各 *.yaml ファイルについて：
     |
     ├─ YAMLを解析
     │     ├─ 解析エラー → 警告 + ファイル全体をスキップ
     │     └─ 成功 ↓
     |
     ├─ ファイル内の各ポリシーについて：
     │     ├─ validate() 失敗 → 警告 +「この」ポリシーをスキップ
     │     └─ validate() 成功 → 保持
     |
     v
ファイル名 stem = agent_id
     └─ stem が [a-zA-Z0-9\-_] チェックを通らない
           → 警告 + ファイルをスキップ（パストラバーサル防御）
```

具体的には：`limit: 0`（不正）のポリシー1つと `limit: 100`（有効）のポリシー1つを含む `global.yaml` は、ちょうど**1つ**——良い方——を読み込みます。完全に解析不能な `global.yaml` は**0個**のポリシーを読み込み、システムはpanicせず空のガバナンスルールで継続します。ディレクトリが存在しない場合もエラーではなく、同様に空のポリシー集合を生みます。

ファイル名ガードはセキュリティ境界です：agent_idはファイル名に由来するため、`../evil.yaml` や `agent.name.yaml` という名のファイルは、その `file_stem` が検索キーとして使われる前に拒否されます。

---

## 再起動なしのホットリロード

registryは `notify` で自身のディレクトリを監視できます（Linuxではinotify、macOS/Windowsでは相当の実装）。YAMLファイルが作成・変更・削除されると、registryは非同期にリロードします：

```
オペレーターが policies/global.yaml を編集
     |
     v
notify が Create/Modify/Remove イベントを発火
     |
     v
イベントパスは .yaml で終わるか？
     ├─ いいえ → 無視
     └─ はい → tokio::spawn(registry.load())
              "Policy file changed, reloading..."
```

リロードは同じフェイルセーフ経路を通るため、稼働中の誤編集も優雅に劣化します——不正なファイルはスキップされ、メモリ内の従前の有効集合は新しい有効集合にのみ置き換えられます。再起動なし、ダウンタイムなし、ルールブックが中途半端に適用されることもありません。

---

## Quota：ソフトな追跡、ハードな執行

`QuotaManager` は、evaluatorが参照する独立したエージェント単位の使用量トラッカーです。`QuotaPolicy` に対して3つの消費次元を執行し、その執行は**ハード**です——予算を超えると警告ではなく型付きの `QuotaError` を返します。

```
consume_tokens(agent, policy, tokens)
     |
     v
needs_reset()？（Utc::now() >= reset_at）
     ├─ はい → カウンターをリセット、次の 00:00 UTC を予定、
     │          governance_quota_reset イベントを発火
     └─ いいえ ↓
     |
     v
new_total = token_used + tokens
     |
     ├─ new_total > daily_token_budget
     │     → Err(TokenBudgetExhausted { used, budget })   ← ハード停止
     │
     └─ new_total <= daily_token_budget
           → token_used = new_total                        ← OK
```

境界は厳密です：消費が拒否されるのは、累計が予算を**厳密に超える**場合のみです。予算に*ちょうど*到達するのは許容されます——最後の正当なトークンはまだ収まります。しかし `token_used >= budget` になると `check_token_budget()` は枯渇を報告し、以後いかなる正値の消費も成功しません。2つの視点は一貫して保たれます。

3つのquota次元：

| 次元 | メソッド | 超過時のエラー |
|------|----------|----------------|
| 日次トークン | `consume_tokens` / `check_token_budget` | `TokenBudgetExhausted { used, budget }` |
| 並行タスク | `increment_concurrent_tasks`（`decrement_concurrent_tasks` と対） | `ConcurrentTasksExceeded { current, max }` |
| メモリ条目 | `set_memory_entries` | `MemoryEntriesExceeded { current, max }` |

カウンターはエージェント単位で隔離されています——`agent-a` が自分の予算を使い切っても `agent-b` には決して触れません。日次リセット時刻は翌日の `00:00` UTC として計算されます。リセット時には fire-and-forget で `governance_quota_reset` 監査イベントが発火されます（`reset_type` は `daily`、`manual`、または一括の `"*"`）。

---

## エラーコード：違反からHTTPへ

evaluatorが操作を拒否すると、違反は安定した `PolicyErrorCode` と固定のHTTPステータスにマッピングされます——呼び出し側とダッシュボードが一貫した機械可読なレスポンスを得られるように：

| エラーコード | HTTP | 意味 |
|--------------|------|------|
| `POLICY_RATE_EXCEEDED` | 403 | レート制限超過 |
| `POLICY_PERMISSION_DENIED` | 403 | scopeが許可されていない |
| `POLICY_QUOTA_EXCEEDED` | 403 | 日次クォータ枯渇 |
| `POLICY_LIFECYCLE_VIOLATION` | 403 | ライフサイクルポリシー違反 |
| `POLICY_NOT_FOUND` | 404 | ポリシーが存在しない |
| `POLICY_CONFLICT` | 409 | ポリシー競合（同一id、異なるtype） |
| `POLICY_INVALID_SCHEMA` | 422 | スキーマ検証失敗 |
| `POLICY_APPROVAL_REQUIRED` | 202 | 操作が承認待ちにキュー（エラーではない） |

`POLICY_APPROVAL_REQUIRED` は4xxではなく `202 Accepted` である点に注意してください——承認を要する操作は*保留中*であって*拒否*ではありません。`warn` レベルのレートルールに触れただけで許可された操作は、APIエラーをまったく生みません。

---

## 完全なポリシーファイル

同梱の `policies/global.yaml` は、すべてのエージェントが継承するデフォルトルール集を宣言し——1つのファイルで4タイプすべてを実演します：

```yaml
policies:
  # ── Rate：毎分200回のMCP呼び出し ──
  - policy_type: rate
    policy_id: default-rate-mcp
    agent_id: "*"
    resource: mcp_calls
    limit: 200
    window_seconds: 60
    action_on_violation: reject

  # ── Quota：日次500kトークン、5並行タスク ──
  - policy_type: quota
    policy_id: default-quota-daily
    agent_id: "*"
    daily_token_budget: 500000
    max_concurrent_tasks: 5
    max_memory_entries: 10000
    reset_cron: "0 0 * * *"

  # ── Permission：adminを拒否、agent CRUDは承認制 ──
  - policy_type: permission
    policy_id: default-permission
    agent_id: "*"
    allowed_scopes:
      - memory:read
      - memory:write
      - wiki:read
      - wiki:write
      - messaging:send
      - mcp:call
    denied_scopes:
      - admin
      - governance:write
    requires_approval:
      - agent:create
      - agent:modify
      - agent:remove

  # ── Lifecycle：48時間アイドルで自動サスペンド ──
  - policy_type: lifecycle
    policy_id: default-lifecycle
    agent_id: "*"
    max_idle_hours: 48
    health_check_interval_seconds: 300
    auto_suspend_on_violation_count: 10
```

単一エージェントのMCPレート上限を引き上げるには、`limit: 500` の `default-rate-mcp` ポリシーを含む `policies/duduclaw-eng-infra.yaml` を置くだけです——それはそのエージェントについてのみグローバル上限を上書きし、グローバルのpermissionとquotaポリシーは引き続き継承されます。

---

## なぜ重要か

### 宣言的、ハードコードではない

制限値はコード各所に散らばるマジックナンバーではなく、YAMLに存在します。`global.yaml` はすべてのデフォルト上限の唯一の真実の源——監査可能で、バージョン管理され、再コンパイルなしに編集できます。

### 設計上フェイルセーフ

ガバナンスはプロジェクトの「セキュリティゲートはフェイルクローズ」ルールに従います。不正形式のポリシーは致命的ではなくスキップされ、欠落したルールブックはpanicではなく空ルールを生みます。1つの誤編集がゲート全体をオフラインにすることは決してありません。

### フォークなしの上書き

エージェント単位のファイルは `(policy_id, type)` でグローバルデフォルトを上書きします——強力なエージェントは高い上限を、隔離中のエージェントはより厳しい上限を得て、いずれも共有の `global.yaml` に触れる必要はありません。同一id・異なるtypeのポリシーは互いを潰さず共存します。

### 一貫した機械可読な拒否

すべての違反は1つの安定したエラーコードとHTTPステータスにマッピングされます。ダッシュボード、MCPクライアント、監査ログはすべて同じ語彙を見ます——`POLICY_QUOTA_EXCEEDED` は常に同じ意味で、常に403を返します。

### 稼働運用のためのホットリロード

インシデント中にレート上限を絞るのは、再デプロイではなく編集して保存するだけです。watcherが変更を捉え、新しい上限は次の評価で適用されます。

---

## まとめ

建物にはルールブック（何を建ててよいか）とメーター（どれだけ使ってよいか）の両方が必要です。DuDuClawのガバナンスレイヤーはその両方を、すべてのエージェントの前に備えます：PermissionとLifecycleポリシーが*何が許可されるか*を、RateとQuotaポリシーが*どれだけ許可されるか*を語ります。そのすべてがフェイルセーフなYAMLで宣言され、エージェントがグローバルを上書きする形で解決され、再起動なしにリロードでき、一貫したエラー語彙で拒否されます。これは、信頼できず自己進化するエージェントを、盲目的に信頼することなく大規模に運用させてくれるゲートです。
