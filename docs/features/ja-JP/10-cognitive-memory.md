# 認知メモリシステム

> 人間の記憶をモデルにした、忘却曲線を持つメモリです。エージェントは重要なことを覚え、重要でないことを忘れます。

---

## たとえ：脳はどのように記憶を整理するか

物事をどう覚えているかを考えてみてください。

- **「先週の火曜日に Sarah とコーヒーを飲んだら、転職すると言っていた。」**：これは**エピソード記憶**（episodic memory）で、特定の時点に起きた具体的な出来事です。
- **「Sarah はマーケティング部門で働いている。」**：これは**意味記憶**（semantic memory）で、特定の出来事から切り離された一般的な事実です。

脳はこの 2 つを自然に分けています。「Sarah の仕事は何？」と聞かれたとき、彼女との会話をすべて再生したりせず、事実に直接アクセスします。

そして時間が経つと、重要でないエピソード記憶は薄れていきます。2 週間前の火曜日の昼食は覚えていませんが、上司から昇進を告げられた昼食は覚えています。それが*重要*だったからです。

DuDuClaw のメモリシステムはこの構造を模倣しています。

---

## 仕組み

### 2 つのメモリストア

**エピソード記憶**：具体的な出来事の記録で、各エントリには次の情報が保存されます。
- タイムスタンプ（いつ起きたか）
- タグとソースイベント（何によって生成されたか。例：予測の観測）
- 重要度（0〜10）とアクセス履歴（何回、どれだけ最近に想起されたか）

エントリの例：
```
[2026-04-05 14:30] User asked about Rust lifetimes in Discord.
  Struggled with 'static lifetime. Explained with analogy.
  importance: 4

[2026-04-06 09:15] User reported a bug in the billing module.
  Root cause: null check missing in invoice calculation.
  importance: 8
```

**意味記憶**：時間的な文脈を持たない、抽出された事実と知識です。
```
User is a backend developer focused on Rust.
User prefers analogy-based explanations.
The billing module has a history of null-related bugs.
```

### メモリ検索：3 次元加重検索

エージェントが何かを想起するとき、キーワード検索だけでなく、3 つの次元でメモリの関連度をランク付けします。

```
Query: "Help me with a Rust lifetime issue"
     |
     v
For each memory entry, compute:
     |
     +---> Recency: How recently was this memory created/accessed?
     |       (Recent memories score higher)
     |
     +---> Importance: How significant was this event?
     |       (Critical decisions > casual chat)
     |
     +---> Relevance: How closely does it match the query?
             (Full-text keyword rank)
     |
     v
Final score = weighted combination of all three
     |
     v
Return top-N memories, sorted by score
```

重みはエンジン内の固定の既定値（新しさ 0.25、重要度 0.35、キーワード関連度 0.35）で、エージェントごとには設定できません。さらに 2 つのシグナルがスコアに加算されることがあります。ナレッジグラフのシグナル（0.15。保存済みの主語–述語–目的語の事実に対する Personalized PageRank）と、ベクトル類似度のシグナル（0.15。embedder が接続されている場合）です。最後に、各スコアはメモリの由来の信頼度に応じてスケーリングされます（重み 0.10）。

この手法はスタンフォード大学の **Generative Agents** 論文に着想を得ています。同論文は、3 次元検索が単純なキーワード検索よりも人間らしい想起を生むことを示しました。

### メモリの減衰：忘却曲線

すべての記憶が永遠に残るべきではありません。新しさのスコアは **Ebbinghaus の忘却曲線**に従い、毎日実行されるジョブが薄れた記憶をアーカイブします。

```
Memory created
     |
     v
  Retrievability R = exp(-t / S)
  (t = days since last access, R starts at 1.0)
     |
     v
  Time passes without access...
     |
     v
  R decays toward 0
     |
     v
  Older than 30 days, importance below 3,
  not semantic, and R below 0.05?
     → Moved to the archive
     (No longer returned by retrieval;
      deleted after 90 days in the archive)
     |
     v
  If accessed again → t resets, so R is back to 1.0,
     and stability S grows with every access
```

安定度 `S` は重要度と想起回数で決まります。
- **重要度が高い**（重要度 10 で最大 2 倍）：減衰が遅くなります。重要度 3 以上はアーカイブされません
- **頻繁に想起される**：`S` は `ln(1 + access_count)` に応じて増え、上限は 365 日です
- **重要度が低く、一度も想起されない**：減衰が最も速くなります（基本安定度 14 日を縮小）
- **意味記憶**はアーカイブされません

これにより、メモリストアが際限なく増えることを防ぎます。古く重要でない記憶は自然に薄れ、検索は高速で焦点の合った状態に保たれます。

---

## 全文検索

キーワードで直接検索する場合、システムはデータベースに組み込まれた全文検索を使います。

```
User: "Find everything about the billing bug"
     |
     v
Full-text search index scans all memory content
     |
     v
Returns matches ranked by relevance
  - "User reported a bug in the billing module..."
  - "The billing module has a history of null-related bugs..."
  - "Fixed billing calculation for edge case..."
```

これは 3 次元加重検索を補完します。探している*もの*がわかっているときは全文検索、文脈に合った想起が必要なときは 3 次元加重検索を使います。

### ベクトル類似度

キーワードを丸ごと共有していなくても似ている記憶を見つけるために、エンジンは embedding ベクトルを比較できます。

```
Query: "invoice calculation error"
     |
     v
Convert to embedding vector
     |
     v
Cosine similarity against the agent's embedded memories
     |
     v
Results include memories about:
  - "billing module null check" (semantically related)
  - "price rounding issue in orders" (similar domain)
  - "tax calculation edge case" (conceptually adjacent)
```

同梱の embedder はローカルの文字 n-gram ハッシュ embedder（モデルのダウンロード不要）です。そのため、ニューラル embedding モデルのように意味を比較するのではなく、重なり合う語の断片（CJK テキストを含む）を照合します。比較は全件走査で、別途のベクトルインデックスはありません。gateway のメモリエンジンでは `[memory] novelty_gate` が有効なとき（既定で有効）に接続され、MCP のメモリツールでは `DUDUCLAW_SEMANTIC_VECTORS=1` が設定されている場合にだけ接続されます。

---

## エージェント間の知識共有

gateway が書き込むメモリ（会話とプロフィールの抽出、承認済みの審査、重要事実）はエージェント単位です。MCP のメモリツール（`memory_search`、`memory_store`、`memory_read` など）は MCP キーに対応する名前空間を読み書きします。gateway が起動する従業員はすべて gateway の内部キーを使うため、これらのツールを通すと同じ gateway の全従業員が1つの名前空間（`internal/gateway-internal`）を共有し、各従業員の gateway が書き込んだメモリとは別になります。これは既知の制限です（[20-memory-intelligence.md](20-memory-intelligence.md#既知の制限2つのメモリ名前空間v1671-では未修正) を参照）。メモリごとの共有レベルはありません。複数のエージェントが必要とする知識は、代わりに共有 wiki に置きます。

```
Agent A (customer support) needs product info
     |
     v
Search the shared wiki (wiki_search scope="shared")
     |
     v
Visibility check:
  Does wiki_visible_to allow this agent?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  v     v
Return  Not
result  visible
```

知識は 2 つのレベルに置かれます。
- **エージェントのメモリとエージェントの wiki**：所有するエージェントのみ
- **共有 wiki**（`~/.duduclaw/shared/wiki/`）：`wiki_visible_to` 権限で許可されたエージェント

これは組織が情報を扱う方法と同じです。部門固有の知識もあれば、全社共通の知識も、知る必要のある人だけの知識もあります。

---

## Wiki ナレッジベース

会話の記憶とは別に、システムは構造化された知識を wiki ページとして保持します。

```
Knowledge source
  (wiki_write by an agent, operator edits,
   reference documents auto-filed from conversation)
     |
     v
Wiki page:
  - Markdown with frontmatter
  - Agent-local or shared scope
  - Indexed for full-text search
     |
     v
Knowledge base (searchable with wiki_search)
```

ダッシュボードのナレッジハブページには、wiki ページが共通のトピックを通じてどうつながっているかを示す**関連グラフ**があります。[Wiki ナレッジレイヤー](17-wiki-knowledge-layer.md) を参照してください。

---

## なぜ重要なのか

### パーソナライズされた対話

メモリを持つエージェントは、毎回の会話をゼロから始めません。ユーザーの好み、過去の問題、コミュニケーションスタイルを覚えています。体験は「毎回初対面の人と話す」から「自分を知っている人と話す」に変わります。

### 知識の蓄積

時間とともに、エージェントは自分の領域について深い理解を築きます。サポートエージェントは、よくある問題、既知の回避策、ユーザーごとの設定に関する知識を蓄積します。この知識はセッションをまたいで保持され、時間とともに応答の質を高めます。

### スケーラブルなメモリ

忘却曲線により、メモリが際限なく増えることはありません。システムは関連性が高く新しい記憶の作業セットを自然に維持し、古く重要でない記憶を薄れさせます。手動の整理は不要です。

### エージェント横断のインテリジェンス

共有 wiki があることで、知識は一つのエージェントに閉じ込められません。あるエージェントが書き込んだ製品の知見は、オペレーターが設定した可視範囲の中で、サポート、営業、ドキュメントのエージェントに役立ちます。

---

## 他システムとの連携

- **予測エンジン**：チャネル返信の後にエピソードの観測（`source_event = prediction_episodic`）を書き込みます。
- **会話の蒸留**：会話中の事実は意味記憶になり、参照ドキュメントは wiki ページになり、メモリには短いポインタだけが残ります。
- **メモリインテリジェンス**：時間的な置き換え、reflexion ルール、由来の信頼度はこのエンジンの上に構築されています。[メモリインテリジェンス](20-memory-intelligence.md) を参照してください。
- **ダッシュボード**：メモリの内容、検索、ナレッジハブのグラフは Web インターフェースから利用できます。

---

## まとめ

メモリは、状態を持たないチャットボットと役に立つアシスタントを分けるものです。エピソード記憶と意味記憶の分離、重要度で重み付けした検索、自然な忘却、共有ナレッジベースという人間の認知にならった設計により、DuDuClaw はエージェントがあらゆる対話から学び、記憶し、成長できるようにします。
