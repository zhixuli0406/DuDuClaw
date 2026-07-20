# メモリインテリジェンス

> 互いに置き換わる事実、ルールへと昇華する間違い、ひとまとめで取り出す想起——スキーマを書き換えずに現役のメモリエンジンへ重ねた3つの強化。

---

## たとえ話：医師のカルテ

優れた医師はカルテを平坦なメモの山として扱いません。3つの結びついた習慣として使いこなします：

1. **事実には時間軸がある。**「患者はその薬を10mg服用している」は真である——投与量が変更される*まで*は。新しい投与量が記録されると、古い行は消されず「3月3日まで有効」と刻印され、新しい行が引き継ぐ。「去年の冬の投与量は？」と尋ねれば、カルテは歴史の正しい瞬間から答える。
2. **間違いはプロトコルになる。**ある薬物相互作用を3度目に見落とした後、診療所はその1件を直すだけでなく、常設ルールを書き残す——「常に相互作用Xを確認せよ」。次の医師が読むのはそのルールであり、3通のインシデント報告ではない。
3. **想起はひとまとめで行う。**症例を見直すとき、医師は必要なページを参照番号で正確に引き出す——バインダー全体を1枚ずつ読み返したりはしない。

DuDuClawの**メモリインテリジェンス**（v1.19.0）は、エージェントに同じ3つの習慣を与えます——既存の`SqliteMemoryEngine`の上に**非侵襲的**に構築（スキーマの書き換えなし、`MemoryEntry`は不変）。

---

## 3つの機能

| | 機能 | 役割 | 所在 |
|-|------|------|------|
| **F1** | Temporal Memory | 事実が有効期間 + 知識グラフのトリプルを獲得；新しい事実が古いものを置き換えチェーンを連結 | `engine.rs` — `store_temporal`、`get_history`、`get_at` |
| **F2** | Reflexion Loop | 最近の未解決の間違いをプロンプトに注入（F2a）；同カテゴリ ≥3 件の間違いを1つの semantic ルールに統合（F2b） | `channel_reply.rs`、`reflexion.rs`、`MistakeNotebook` |
| **F3** | Batch Fetch | 1回の呼び出しで最大100件のメモリをIDで取得、部分ヒット時は `missing_ids` を返す | `engine.rs` — `get_by_ids`；MCP `memory_fetch_batch` |

3つとも現役エンジン上に実装——マイグレーションは再構築ではなく、**冪等な ALTER ループ**です。

---

## F1：Temporal Memory

### 新しいカラム（冪等マイグレーション）

マイグレーションループは、既存の行に対して `ALTER TABLE ... ADD COLUMN` が合法となるよう、NULL 可／定数デフォルトの9カラムを追加し、さらに2つのインデックスを作成します：

| カラム | 意味 |
|--------|------|
| `valid_from` | 事実が真になった時刻（NULL ⇒ `timestamp` にフォールバック） |
| `valid_until` | 事実が真でなくなった時刻（NULL ⇒ 今も有効） |
| `superseded_by` | この行を置き換えた行の id |
| `supersedes` | この行が置き換えた行の id |
| `subject` / `predicate` / `object` | 知識グラフのトリプル |
| `confidence` | 0.0–1.0、デフォルトは 1.0 |
| `metadata` | JSON ブロブ、デフォルトは `{}` |

```sql
-- F1 Temporal Memory columns (v1.19.0) — all nullable / constant-default
ALTER TABLE memories ADD COLUMN valid_from    TEXT;
ALTER TABLE memories ADD COLUMN valid_until   TEXT;
ALTER TABLE memories ADD COLUMN superseded_by TEXT;
ALTER TABLE memories ADD COLUMN supersedes    TEXT;
ALTER TABLE memories ADD COLUMN subject       TEXT;
ALTER TABLE memories ADD COLUMN predicate     TEXT;
ALTER TABLE memories ADD COLUMN object        TEXT;
ALTER TABLE memories ADD COLUMN confidence    REAL NOT NULL DEFAULT 1.0;
ALTER TABLE memories ADD COLUMN metadata      TEXT NOT NULL DEFAULT '{}';

-- Triple index only covers currently-valid rows (cheap conflict lookup)
CREATE INDEX IF NOT EXISTS idx_memories_triple
    ON memories(agent_id, subject, predicate) WHERE valid_until IS NULL;
CREATE INDEX IF NOT EXISTS idx_memories_valid
    ON memories(agent_id, valid_until);
```

ループは `duplicate column name` エラーを飲み込むため、既にアップグレード済みのデータベースで再実行しても no-op です。

### 自動コンフリクト解決

`store_temporal(entry, TemporalMeta)` が `subject` と `predicate` の**両方**を伴って呼ばれると、エンジンは `(agent_id, subject, predicate)` を事実の同一性として扱います。同じトリプルを持つ現在有効な行は、新しい行を挿入する前にクローズされます：

```
store_temporal(agent="dudu",
               subject="user", predicate="deploy_target",
               object="Cloudflare Workers")
     |
     v
(dudu, user, deploy_target) の現在有効な行を検索
     |
   見つかった？ ──いいえ──> 新しい行をそのまま INSERT（valid_until = NULL）
     |
    はい
     |
     v
古い行を UPDATE：  valid_until = now
                   superseded_by = <新しい id>
     |
     v
新しい行を INSERT：supersedes = <古い id>
                   valid_until = NULL   （現在有効）
```

2つの行は **置換チェーン（supersession chain）** に連結されます：

```
[ deploy_target = Vercel ]      [ deploy_target = Cloudflare Workers ]
  valid_from  : Jan 1            valid_from  : Mar 3
  valid_until : Mar 3   ───────► valid_until : NULL  （現在）
  superseded_by ──────────┘      supersedes ─────────┘
```

完全なトリプルがない場合、`store_temporal` はタイムスタンプ付きの事実を記録するだけです——置換は発生しません。

### デフォルトで「現在有効」をフィルタ

`search()` / `search_layer()` はすべてのクエリに `AND (m.valid_until IS NULL OR m.valid_until > now)` を追加するため、通常の取得は*今*真である事実だけを返します。古い事実は履歴のためデータベースに残りますが、プロンプトに漏れることは決してありません。

### 時間軸を読む

2つの読み取り API がチェーンを露出し、いずれも MCP として `memory_get_history` / `memory_get_at`（scope `memory:read`）でも利用できます：

| API / MCP ツール | 返り値 |
|-----|--------|
| `get_history(agent, subject, predicate)` — `memory_get_history { subject, predicate }` | 完全な置換チェーン、古い順 → 新しい順。各レコードの `ingested_at`、`invalidated_by_event`/`invalidated_at`、`reaffirmed_by` を含む |
| `get_at(agent, subject, predicate, at)` — `memory_get_at { subject, predicate, at }` | ある時点で有効な単一の事実（`valid_from <= at AND (valid_until IS NULL OR valid_until > at)`） |

### バイテンポラル + ビルド時プロビナンス（D1）

時系列ストアは**2つ**の時間軸を追跡します：`valid_from`/`valid_until`（world-time、事実が真である期間）と `ingested_at`（transaction-time、システムがそれを知った時刻）です。置換は取り込み順ではなく world-time の `valid_from` で決まるため、取り込み順が乱れても（先に離婚を、後からより早い結婚を知る）、任意の時点で正しい事実を解決できます：`valid_from` が現行の事実より前の1件は、現行の事実を乱さずに有界の*履歴セグメント*として挿入されます。同一の事実（同じ subject/predicate/object ＋内容）を再観測すると、新しい行を作らずにそれを**再確認（reaffirm）**します：新しい `source_event` を `reaffirmed_by`（上限 20）に追加し、`access_count` を加算します。事実がクローズされると、クローズに用いた `source_event` と時刻が被置換行に刻印されます（`invalidated_by_event`/`invalidated_at`）。

### ソースロールバック（`memory_invalidate_by_origin`）

`invalidate_by_origin(agent, origin, since)`（MCP `memory_invalidate_by_origin`、scope `admin`）は、汚染されたソースへの是正弁です：**厳密な** `origin`（部分文字列ではなく等値）のすべての現在有効な事実を失効させ（削除は決してしない）、任意で `since` 以降に知った事実に限定できます。`derived_from` が削除された id を参照する事実は、その `origin_trust` が ≤ 0.1 に切り下げられます（汚染された入力の派生物は信頼され続けられません）。`search()` はこれら削除された事実の返却を即座に停止し、`get_history()` は `invalidated_by_event = "origin_purge"` として完全なチェーンを保持します。

### 書き込み側の汚染防護（D2）

D1 は汚染されたソースを*取り消す*ことができます；D2 はほとんどの汚染をそもそも入り込ませません（PoisonedRAG、arXiv:2402.07867）。自動蒸留の書き込みパスは両端で守られます：

- **書き込み側スキャン + バースト検知。**蒸留された事実が保存される前に、その内容と `(subject, predicate, object)` が共有のプロンプトインジェクションルールエンジンを通ります：一致すればその事実は**破棄**され（fail-closed、決して書き込まれない）、`prompt_injection` セキュリティ監査イベントが記録されます。別途、per-`(agent, origin, subject)` のスライディングウィンドウカウンタ（`knowledge_guard`、dispatch ブレーカーと同じ永続化 + advisory-lock パターン）があり、単一のオリジンがウィンドウ内で同じ subject について `>= max_per_subject` 件の事実を書き込むと、バッチを隔離します（「One Shot Dominance」／k-doc パターン）。隔離された事実は `quarantined = 1` で保存され、**不活性**です：クリーンな事実を決して置換せず、人間が判断するまですべての取得読み取りパス（FTS、graph、vector、`list_recent`、`summarize`）から除外されます。
- **処理。**隔離は `ApprovalBroker` リクエスト（`action_kind = "knowledge_quarantine"`）を発行し、`knowledge.quarantined` イベントを送出します。承認 → 事実は解放されます（`quarantined = 0`、取得可能に）；拒否 → 事実は失効し（`invalidated_by_event = "quarantine_reject"`）、その `origin_trust` が ≤ 0.1 に切り下げられます；TTL 失効は拒否とみなされます（fail-closed）。

**ランキング側の信頼。**`origin_trust` は取得ランキングに参加するようになりました（重み `w_trust`、デフォルト 0.10）：各候補のスコアは `(1 − w_trust) + w_trust · origin_trust` で乗算されるため、未検証のチャネル蒸留事実（trust 0.3）はキュレートされた事実（trust 1.0）を上回れません。HippoRAG-lite graph では、トリプルのエッジがその `origin_trust` で重み付けされ、低信頼な事実の Personalized-PageRank の質量を縮小します。これは「単一の汚染トリプルが PPR によって2ホップ増幅される」経路を直接抑制します。レガシー行（trust 1.0）は D2 以前のパスとバイト単位で同一にランクされます。

### グラフ検索の進化（D3）

HippoRAG-lite graph は4つの独立した改良を得ました（HippoRAG 2 + LightRAG との整合）。いずれも fail-safe です：エイリアスなし、小さなグラフ、embedding seeding オフのとき、ランキングは以前のクエリ毎ビルドと**バイト単位で同一**です。

- **永続的インクリメンタルグラフキャッシュ。**エージェントが多くの事実を蓄積すると、クエリ毎に Personalized-PageRank グラフを再構築するのは無駄です。グラフは現在エージェント単位でキャッシュされ（`RwLock`）、クエリ間で再利用されます；per-agent の**世代カウンタ**（generation counter）が、トリプルを変更するすべての書き込み（`store_temporal`／supersession、隔離の解放/拒否、origin purge、decision 失効、decay アーカイブ、GDPR 消去、エージェント再割り当て）で加算され、古いキャッシュを無効化するため、クエリは常に現行の事実を見ます。キャッシュは `GRAPH_CACHE_MIN_TRIPLES`（500）を超えたときのみ有効になります；それ未満ではクエリ毎ビルドの方が安価なので維持されます。
- **エンティティエイリアス統合。**`entity_alias(agent_id, canonical, alias)` テーブルが、グラフの構築とシードの前に表層形を1つのノードに畳み込むため、「老闆／李老闆／zhixu」が3つの孤立した島でなくなります。両辺は正規化され（trim + 小文字化）、エイリアスチェーンは保存時に平坦化されます。`memory_alias_add` / `memory_alias_list` MCP ツールで管理します（write／read scope）。エイリアスがなければグラフはバイト単位で同一です。
- **述語エッジラベル。**各 SPO エッジは現在、その predicate をラベルとして付帯します（PPR の計算はそれを読まないので、ランキングは不変です）。`engine.export_graph(agent, limit)` API はシリアライズ可能な `{ nodes, edges }` スナップショット（保留中の隔離事実を含み、フラグ付き）を返し、D6 のナレッジグラフ・キュレーション UI に供給します。
- **Embedding seeding（オプトイン）。**`graph_embed_seed` がオンで**かつ** embedder が接続されているとき、PPR のシードは whole-word FTS のエンティティ一致とクエリ埋め込みの最近傍エンティティベクトル（同一モデルの cosine、top-k）の和集合になります。エンティティベクトルは `entity_embedding` に遅延キャッシュされ、embedding 失敗時は FTS シードにフォールバックします。デフォルトはオフ（embedder がなければ no-op）で、弱い embedder は recall を失うという HippoRAG 2 の注意に従います。

---

## F2：Reflexion Loop

F2 は**既存**の `MistakeNotebook` を応答パスに橋渡しします——新しいストアではありません。トリガー信号は既存の `ErrorCategory`（Significant／Critical、MetaCognition が自己調整）であり——SOUL.md の提案を検証する GVU Verifier では**ありません**。

### F2a — 過去の間違いをプロンプトに注入

エージェントがチャネルメッセージに答える前に、最近の未解決の間違いが `## Past Mistakes to Avoid` ヘッダーの下にプロンプトへ浮上します：

```
チャネルメッセージ到着
     |
     v
空白区切りのキーワードを抽出（≥3 文字、最大 12 個）
     |
   キーワードあり？ ──いいえ──> query_by_agent(agent, 3)   ← CJK 直近フォールバック
     |                                                       （CJK は空白トークンなし）
    はい
     |
     v
query_by_topic(keywords, agent, 3)   ← トピック範囲の想起
     |
   空？ ──はい──> query_by_agent(agent, 3)   ← 直近フォールバック
     |
     v
プロンプトに追加：
  ## Past Mistakes to Avoid
  - <間違い 1 のプロンプトセクション>
  - <間違い 2 のプロンプトセクション>
```

これは `MistakeNotebook` をタスク横断学習に橋渡しし、エージェントが類似トピックで過去の失敗を繰り返すのを止めます——GVU SOUL.md パス内だけではありません。

### F2b — 同カテゴリ ≥3 件の間違いを1つのルールに統合

同じ `MistakeCategory` が `>= DEFAULT_CONSOLIDATE_THRESHOLD`（= **3**）件の未解決項目を蓄積すると、`reflexion::maybe_consolidate` はそれらを単一の **semantic** メモリルールに合成し、ソースを解決済みとしてマークします：

```
エージェントの未解決の間違いを MistakeCategory でグループ化
     |
     v
count_unresolved_by_category(agent, Capability) = 3
     |
   < 3？ ──はい──> 何もしない
     |
   >= 3
     |
     v
query_unresolved_by_category(...)  → MistakeEntry[]
     |
     v
synthesize_rule(category, mistakes)   ← 決定論的、LLM 呼び出しなし
  "Recurring capability issues consolidated from 3 past mistakes.
   Apply extra care: ..."
     |
     v
「1つの」semantic メモリとして保存   （source_event = "reflexion_consolidation"）
     |
     v
mark_resolved(source ids)   ← 元の3件が解決済みに
```

合成は**分離され決定論的**——LLM の往復はありません。散らばった3件のインシデントが、エージェントが今後読む1つの常設ルールに収束します。

```
前：                             後：
  ☒ 間違い A (capability)         ✓ A 解決済み ─┐
  ☒ 間違い B (capability)  ───►    ✓ B 解決済み ─┼─► 1つの semantic ルール
  ☒ 間違い C (capability)         ✓ C 解決済み ─┘   "Apply extra care: ..."
```

---

## F3：Batch Fetch（`memory_fetch_batch`）

コンテキストの再構築は、多くの特定エントリを id で取り出すことを意味する場合が多いです。MCP 呼び出しを1件ずつ行うのは遅く冗長です。`get_by_ids`（エンジン）と `memory_fetch_batch` MCP ツールは、1回の呼び出しで最大 **100** 件を取得します：

```
memory_fetch_batch { "ids": ["m_1", "m_2", "m_404", ...] }   （上限 100）
     |
     v
get_by_ids(namespace, ids)
  SELECT ... FROM memories WHERE agent_id = ? AND id IN (?,?,?...)
     |  （namespace／所有権を強制——別の namespace に
     |   属する項目は存在しないものと区別不能）
     v
要求された id を分割：
  ヒット   → memories[]
  欠損     → missing_ids[]   ← エラーではない
     |
     v
{ "memories": [...], "missing_ids": ["m_404"],
  "total_found": N, "total_missing": M }
```

主要な性質：

- **ハード上限 100**——`ids` が 100 を超えると拒否され、暴走クエリを防ぎます。
- **部分ヒットはエラーではない**——ヒットした項目が `missing_ids` リストとともに返ります。
- **存在性を漏らさない**——別の namespace に属する項目も存在しない id も、ともに `missing_ids` に入ります。呼び出し側は他のエージェントが何を所有するか探れません。

---

## 設定

有効化するものは何もありません。メモリインテリジェンスは既存のメモリエンジンに相乗りします：

- **F1** は呼び出し側が `subject` + `predicate` を `store_temporal` に渡した瞬間に有効化されます；通常の保存は不変です。
- **F2a** は channel-reply パスに `ctx.mistake_notebook` が存在する限り発火します。
- **F2b** は `DEFAULT_CONSOLIDATE_THRESHOLD = 3` を使用します。
- **F3** は `memory_fetch_batch` MCP ツールとして露出し、他のすべてのメモリツールと同様に scope でゲートされます。

マイグレーションはエンジン初期化時に自動実行されます——既存のデータベースは冪等な ALTER ループによってその場でアップグレードされます。

### 書き込み側の汚染防護（D2）

書き込み側のバースト検知器はデフォルトでオンで、`config.toml` で調整できます。セクションが欠落または不正な場合は以下のデフォルトにフォールバックします（fail-safe、検知器はオンのまま）：

```toml
[knowledge_guard]
enabled = true          # 同一オリジンのバースト検知器のマスタースイッチ。デフォルト true
window_secs = 3600      # スライディングウィンドウ長（秒）。デフォルト 3600（1 時間）
max_per_subject = 5     # 1 つのオリジンがウィンドウ内で同一 subject に書き込める事実の上限。超えると隔離。デフォルト 5
```

書き込みパスのインジェクションスキャンは無条件です（設定なし）。ランキングの信頼重み `w_trust`（デフォルト 0.10）は `RetrievalWeights`（エンジン単位、config キーではない）にあります；`w_trust = 0.0` のときランキングは D2 以前のパスとバイト単位で同一です。

---

## なぜ重要か

### 事実がひそかに古びなくなる

F1 以前は、ユーザーがとっくに Cloudflare へ移っても、メモリは永遠に「デプロイ先は Vercel」と言い続けました。今では古い事実はクローズされ、新しい事実が引き継ぎ、通常の検索は*今*真であるものだけを返します——一方で履歴は `get_history` / `get_at` で照会可能なまま残ります。

### 間違いが能力に複利する

F2 は予測エンジンのエラー信号とエージェントの将来の行動の間でループを閉じます。間違いは記録されるだけでなく——類似トピックで浮上し（F2a）、再発すれば常設の semantic ルールへ硬化します（F2b）。モデルを変えずにエージェントが上達します。

### 往復税なしの想起

F3 は N 回の冗長な MCP 呼び出しを1回にまとめ、クリーンな部分ヒット契約を備え、namespace 横断の漏洩もありません。コンテキスト再構築が安価になります。

### 設計からして非侵襲

これらはスキーマの書き換えも新しい `MemoryEntry` も必要としませんでした。NULL 可の9カラム、2つのインデックス、1つの冪等マイグレーション、そして既に存在していた notebook。機能全体が現役エンジンに重なります。

---

## まとめ

平坦なメモの山は何も忘れず、何も学びません。良いカルテはその両方を行います：事実にタイムスタンプを押して古いものを優雅に退役させ、繰り返す間違いを常設プロトコルに変え、必要なページを一度の手で引き出させてくれます。メモリインテリジェンスは、すべての DuDuClaw エージェントにそのカルテを与えます——既に持っていたメモリエンジンの上に構築して。
