# Session Memory Stack

> Instruction Pinning、Snowball Recap、Key-Fact Accumulator——6,500トークンの重量級メモリシステムを置き換えた、3つの軽量レイヤー。

---

## たとえ話：シェフの付箋

料理長はレシピ本を暗記できますが、ディナーの繁忙時間にはそうしません。彼らは3つのクイックリファレンス面に頼ります：

1. **パス（出し口）の上に留められた伝票** — 「12番テーブル：甲殻類なし、デザートはヴィーガン。」皿が厨房を出るたびに目を通す。
2. **オーダー票に走り書きされた進行中のまとめ** — 「ステップ3完了、ソースに塩が必要。」かき混ぜる合間に読み返す。
3. **仕込み台の脇にある小さなカード箱** — 数週間かけて蓄積したメモ（「Parkシェフはパセリの飾りを嫌う」）。あるカードが関連するときだけ取り出す。

これらの面はいずれもレシピ本そのものではありません。それらは*安価で、荷重を担う面*であり、シェフの注意がもともと通る場所にちょうど置かれています。DuDuClawのsession memory stackは同じ発想で構築されています。

---

## 解決すべき問題

DuDuClawは一時期、MemGPTに着想を得た3層メモリシステム（Core Memory、Recall Memory、Archival Bridge）を出荷していました。動作はしましたが：

- **プロンプトごとに6,500トークンの肥大化** — 短い会話でも完全なメモリ税を支払っていた。
- **「lost in the middle」による注意の劣化** — 長い注入ブロックは応答品質を改善せず、むしろ低下させた。
- **MCPツール配管に手動呼び出しが必要** — `core_memory_append`、`recall_search`など——をエージェントはしばしば呼び忘れた。

v1.8.1でその全1,985行を削除。v1.8.6では、合わせて約87%安く、モデルがもともと注意を向ける位置に座る3つの軽量な面に置き換えました。

---

## レイヤー1：Instruction Pinning（v1.8.6 P0）

session内の最初のユーザーメッセージには通常*核心となるタスク*が含まれます。その後はすべて明確化です。そこで：

```
Turn 1: "Help me migrate this React app from CRA to Vite,
         keep the existing tests, and don't touch the auth flow."
     |
     v
Async Haiku extraction:
   "migrate React app CRA → Vite; preserve tests; don't touch auth flow"
     |
     v
Stored in: sessions.pinned_instructions (SQLite column)
     |
     v
Injected at: system prompt tail (high-attention U-shape position)
```

抽出は*非同期*で実行され——最初の応答をブロックしません。これは**メタデータタスク（metadata task）**なので、CLI軽量パス（`--effort medium --max-turns 1 --no-session-persistence --tools ""`）を通常コストの約25-40%で使用します。

### 明確化の蓄積

エージェントが明確化の質問（「service workerは保持すべきですか？」）をしてユーザーが答えると、その回答はpinned instructionsに追記されます——ドリフトを防ぐため1,000文字に上限が設けられています。

```
Pinned instructions grow with clarifications:
  "migrate React app CRA → Vite; preserve tests;
   don't touch auth flow;
   [+] keep service worker behavior identical;
   [+] target Node 20 runtime"
```

### なぜsystem promptの末尾か？

LLMはコンテキストウィンドウの先頭と末尾に不釣り合いなほど注意を向けます（U字型）。system promptの末尾は最も注意の高いスロットの一つです。Instruction Pinningはタスク記述をまさにそこに置きます——毎ターン、毎呼び出しで。

---

## レイヤー2：Snowball Recap（v1.8.6 P0）

各ターンは、ユーザーメッセージの前に`<task_recap>`ブロックを付加します：

```
<task_recap>
Pinned task: migrate React app CRA → Vite; preserve tests;
             don't touch auth flow
</task_recap>

Actual user turn: "what about the proxy config?"
```

「snowball（雪だるま）」という名前は、recapがLLMに記憶を促す再プロンプトなしに会話全体で自然に蓄積されるという事実に由来します。LLM呼び出しコストはゼロ——純粋な文字列連結です。

U字型の注意の末尾効果と組み合わせることで、追加のLLMラウンドトリップなしに、モデルが一つひとつのターンでタスクを「見る」ことを意味します。

---

## レイヤー3：P2 Key-Fact Accumulator（v1.8.6）

一部の事実は一つのsessionに固有ではなく——時間をまたいでユーザーやプロジェクトを記述します。例：

- 「ユーザーのデプロイ先はCloudflare Workers」
- 「好みのテストライブラリはjestではなくvitest」
- 「コードベースは`pnpm`を使い、`npm i`は決して使わない」

MemGPTのCore Memoryはこれらを捉えようとしていましたが、ターンあたり約6,500トークンでした。Key-Fact Accumulatorは約100-150トークンでそれを実現します。

### 仕組み

```
Each substantive turn (non-trivial content)
     |
     v
Async Haiku extraction (lightweight CLI path):
   "Extract 2-4 key facts about the user, project, or preferences.
    Skip ephemeral context."
     |
     v
Stored in: key_facts table (FTS5 indexed)
  ┌─────────────────────────────────────────┐
  │ id | agent_id | content | access_count  │
  │ timestamp | source_turn_id              │
  └─────────────────────────────────────────┘
     |
     v
Next turn's system prompt assembly:
  SELECT content FROM key_facts
  WHERE agent_id = ?
  ORDER BY fts5_rank(relevance) DESC
  LIMIT 3
     |
     v
Inject top-3 as ~100-150 tokens
```

注入のたびに`access_count`が加算されます——頻繁に使われる事実は表面に出続け、一度きりの事実は流れ落ちていきます。

### MemGPT Core Memoryとの比較

| | MemGPT Core Memory | Key-Fact Accumulator |
|-|-|-|
| 注入サイズ | ~6,500 tokens | ~100-150 tokens |
| 検索 | プロンプトごとに完全ブロック | FTS5ランキング上位3 |
| 呼び出し | 手動MCPツール | 自動注入 |
| ストレージ | 永続ブロック編集 | 追記 + アクセス追跡 |
| 実質削減 | ベースライン | **−87%** |

---

## ネイティブマルチターンの基盤（v1.8.1）

3つのレイヤーはすべて、固定された**ネイティブsession handle**の上に座ります：

```
Claude CLI --resume <session-id>
     |
     v
session-id = SHA-256(agent_id + channel_id + thread_id)
     |
     v
If --resume fails (stale handle, account rotation,
                   unknown stream-json error):
     ↓ auto-fallback
History-in-prompt (XML-delimited turns)
```

これは、Agnesが連続したメッセージ間でコンテキストを失っていた以前の挙動（「幫我全部開啟」→「你指的是什麼？」）を修正します。session idは決定論的で、*スレッド全体*のライフタイムを通じて安定しています（v1.8.14のDiscord修正以降：`auto_thread && !is_thread`の代わりに`is_thread || created_thread`）。

### Turn Trimming

長い会話ターン（>800文字）は、モデルに送る前にトリミングされます：

```
Original turn: [850 chars of user input]
     |
     v
Trimmed: [first 300 chars] ... [trimmed 350 chars] ... [last 200 chars]
```

CJKセーフな文字レベルのスライス——マルチバイトcodepointのpanicは起きません。LLMコストはゼロ。冒頭の意図と最終的な指示を失うことなく、冗長な貼り付けによるトークン肥大化を防ぎます。

### Direct APIキャッシュ戦略

Direct API（`direct_api.rs`）にフォールバックするとき、リクエストはAnthropicの「system_and_3」プロンプトキャッシュ・ブレークポイント配置を使用します——system promptと、新しい方から3番目のassistantターンにキャッシュ・ブレークポイントを置きます。これによりマルチターン会話で約75%、純粋なsystem promptヒットでは95%以上のキャッシュヒット率が得られます。

---

## 進化エンジンとの連携

session memory stackは進化システムから孤立してはいません：

- **予測エラー**は、モデルが言ったこととpinned taskが予測することを比較します。大きな乖離はGVU反省をトリガーします。
- **Key facts**は`external_factors`に供給されます——ユーザーの修正、好みのシグナル——これらがSOUL.md更新を駆動します。
- **Session圧縮**（50kトークンの閾値）は要約を生成し、新しい会話ターンとしてではなく*system prompt*に注入します。

---

## なぜ重要か

### コスト

軽量CLIパスと、上位3つのkey factsだけを注入することを組み合わせることで、メタデータのオーバーヘッドを総トークン予算の10%未満に抑えます——MemGPTの30-40%に対して。

### 注意の品質

pinned instructionsとkey factsをsystem promptの末尾（高注意）に、snowball recapをユーザーメッセージの先頭（こちらも高注意）に配置することで、すべてのターンでタスク記述が*2つ*の高注意スロットに現れます。モデルは長いコンテキストの中間を探り回ってそれらを見つける必要がありません。

### ツール使用への非依存

旧来のMemGPT設計は、モデルが能動的に`core_memory_append`を呼び出すことを必要としました。エージェントはときどき忘れました。新しいstackは純粋に注入駆動です——モデルが協力的でも気が散っていても動作します。

### 後方互換な劣化

Haiku抽出が失敗しても（rate limit、タイムアウト）、sessionは依然として動作します——そのターンでpinning／factsの恩恵を受けられないだけです。何も壊れません。

---

## まとめ

シェフは皿を出すたびにレシピ本を覚え直したりしません。彼らはパスの上の付箋を見て、進行中のオーダー票にざっと目を通し、ときおり仕込み箱からカードを引き出します。DuDuClawのsession memory stackは同じアーキテクチャです：実際の作業と張り合う重いメモリブロックではなく、高注意の位置に置かれた安価な面なのです。
