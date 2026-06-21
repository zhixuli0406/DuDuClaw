# Agent Client Protocol (ACP/A2A)

> IDE がエージェントと対話する方法を、エージェントがツールと対話する方法と同じにする——stdio JSON-RPC 2.0、探索可能、言語非依存。

---

## たとえ話：レストランの予約専用回線

どのレストランにも2つの入口があります：

- **ダイニングルーム**——客が入ってきて、メニューを眺め、ウェイターとやり取りする場所。
- **予約専用回線**——電話プロトコル。回線の向こう側の予約システムは、ダイニングルームがどんな様子かを知る必要はありません。ただ「午後7時に4人席は空いていますか？」と尋ねて、はい／いいえを得るだけです。

Zed、JetBrains、Neovim のような IDE は、DuDuClaw のチャネルインフラ全体を理解することなく、エージェントに「このタスクを引き受けられますか？」と尋ねたいのです。彼らに必要なのは予約専用回線——クリーンで安定した、プロトコル駆動のインターフェースです。

それが ACP server です。

---

## ACP/A2A とは何か？

**ACP** は Agent Client Protocol の略です——IDE ↔ エージェント通信のための stdio JSON-RPC 2.0 行プロトコルです。**A2A** は関連する「Agent to Agent」プロトコルで、エージェントの探索とタスク交換のためのものです。

両者を合わせることで、IDE（または別のエージェント、CI パイプライン、シェルスクリプト）が、稼働中の DuDuClaw エージェントを一級のサービスとして扱えるようになります——探索し、タスクを送り、ステータスをポーリングし、キャンセルできるサービスです。

DuDuClaw が提供するのは server 側です：

```
duduclaw acp-server
     |
     v
Listens on stdin, writes to stdout (line-delimited JSON-RPC 2.0)
     |
     v
Responds to:
  agent/discover   → return AgentCard
  tasks/send       → queue new task
  tasks/get        → poll task status
  tasks/cancel     → cancel running task
```

v1.8.9 より前は、`duduclaw acp-server` はメッセージを表示して戻るだけのプレースホルダーでした。v1.8.9 でそれを本物の `A2ATaskManager` に接続し、機能するようにしました。

---

## Agent Card

すべての ACP server は自身を記述できます。クライアントが接続すると、`agent/discover` を発行して **Agent Card** を受け取れます——アイデンティティ、能力、スキルを含む JSON ドキュメントです：

```json
{
  "name": "duduclaw-pm",
  "description": "Project manager for DuDuClaw v1.9 roadmap",
  "url": "stdio://duduclaw acp-server --agent duduclaw-pm",
  "version": "1.8.14",
  "capabilities": {
    "streaming": true,
    "multi_turn": true,
    "tool_use": true
  },
  "skills": [
    {
      "name": "task_planning",
      "description": "Break down features into TaskSpec workflows",
      "tags": ["planning", "orchestration"]
    },
    {
      "name": "sprint_review",
      "description": "Summarize sprint outcomes from memory + tasks",
      "tags": ["reporting", "retrospective"]
    }
  ]
}
```

これは A2A の `.well-known/agent.json` 探索エンドポイントと同じドキュメント形式です。IDE はこれをキャッシュし、利用可能なスキルを UI に表示し、リクエストをこの特定のエージェントにルーティングするかどうかを判断できます。

### `.well-known` の生成

HTTP 経由で公開されるエージェント（将来の拡張）のために、DuDuClaw は `.well-known/agent.json` ファイルを出力でき、外部クライアントは先に接続することなくエージェントを探索できます：

```
/etc/duduclaw/agents/dudu/.well-known/agent.json
     ↓
https://your-host.example.com/.well-known/agent.json
```

A2A 互換のクライアントはこれを読み、エージェントの能力を知り、続行するかどうかを判断します。

---

## JSON-RPC ループ

stdio server はシンプルな行区切りの JSON-RPC 2.0 ループです：

```
loop {
    read line from stdin
    parse JSON-RPC 2.0 request
    dispatch to handler:
        agent/discover  → return AgentCard
        tasks/send      → TaskManager.send(params)
        tasks/get       → TaskManager.get(id)
        tasks/cancel    → TaskManager.cancel(id)
    write JSON-RPC 2.0 response to stdout
}
```

stdio 上の JSON-RPC は MCP が使うトランスポートと同じです——すでに MCP server を作ったことがあれば、ACP server の95%はすでに作ったことになります。

### セッション例

```
→ {"jsonrpc":"2.0","id":1,"method":"agent/discover"}
← {"jsonrpc":"2.0","id":1,"result":{
     "name":"duduclaw-pm",
     "version":"1.8.14",
     "capabilities":{"streaming":true,"multi_turn":true,"tool_use":true},
     "skills":[...]
   }}

→ {"jsonrpc":"2.0","id":2,"method":"tasks/send","params":{
     "task":"Draft the v1.9 release notes from the last 50 commits",
     "priority":"high"
   }}
← {"jsonrpc":"2.0","id":2,"result":{
     "task_id":"t_abc123",
     "status":"queued"
   }}

→ {"jsonrpc":"2.0","id":3,"method":"tasks/get","params":{"task_id":"t_abc123"}}
← {"jsonrpc":"2.0","id":3,"result":{
     "task_id":"t_abc123",
     "status":"completed",
     "output":"# DuDuClaw v1.9 Release Notes\n\n..."
   }}
```

---

## A2ATaskManager

`tasks/send`、`tasks/get`、`tasks/cancel` の背後には `A2ATaskManager` が存在します。その役割は：

1. **キューに入れる**——受信したタスクをエージェントの既存タスクシステム（`TaskSpec`、`tasks/` ディレクトリ）にキューイングする。
2. **追跡する**——ステータス遷移（queued → running → completed/failed/cancelled）を追跡する。
3. **ルーティングする**——タスクの実行をエージェントの通常の runtime（Claude / Codex / Gemini / OpenAI-compat）にルーティングする。
4. **公開する**——クライアントがポーリングできるよう、タスクエンベロープに結果を公開する。

つまり、ACP 経由で送信されたタスクは、チャネルや MCP ツール経由で送信されたタスクと**同じ**パイプラインを流れます——単一の信頼できる情報源、Logs/Activity ダッシュボードでの統一された可観測性です。

---

## なぜ IDE 統合が重要か

### Zed

[Zed](https://zed.dev) は、ACP 互換のあらゆるエージェントと対話できる「agent panel」を提供します。`duduclaw acp-server --agent <your-agent>` に向ければ、Zed は次のものへのネイティブアクセスを得ます：
- タスクルーティング（`tasks/send` 経由）
- エディタ内のインラインレスポンス
- IDE 内のマルチターンのフォローアップ

### JetBrains

IntelliJ プラットフォームの AI Assistant は、プラグイン経由で ACP を話せるよう拡張できます。接続すると、エージェントはプロジェクトを閲覧し、dispatcher を流れるリファクタを提案し、worktree 分離レイヤーを通じてコミットを着地させられます。

### Neovim

`nvim-acp` プラグインは stdio 行プロトコルを直接使用します——`duduclaw acp-server` はドロップインのバックエンドです。エディタを離れることなく、コマンドライン駆動のエージェントアクセスが得られます。

### CI/CD パイプライン

パイプラインのステップは、ACP 経由でタスクを送信し、完了までポーリングできます：

```yaml
- name: Generate release notes via DuDuClaw
  run: |
    echo '{"jsonrpc":"2.0","id":1,"method":"tasks/send","params":{"task":"..."}}' \
      | duduclaw acp-server --agent duduclaw-pm
```

HTTP server も認証トークンもポート管理も不要——コンテナ内の stdio だけです。

---

## DuDuClaw が話す3つの Stdio プロトコル

命名が重複するため、整理しておく価値があります：

| プロトコル | 用途 | 方向 | コマンド |
|----------|---------|-----------|---------|
| **MCP** | DuDuClaw のツール（channel、memory、agent、wiki、task……）を AI runtime に公開 | Runtime → DuDuClaw | `duduclaw mcp-server` |
| **ACP/A2A** | 外部クライアント（IDE、パイプライン、他のエージェント）が DuDuClaw にタスクを送信 | IDE → DuDuClaw | `duduclaw acp-server` |
| **Runtime stdio** | DuDuClaw が runtime（Claude/Codex/Gemini）サブプロセスを起動し stdio JSON で対話 | DuDuClaw → Runtime | *内部* |

これらは3つの異なる会話で、すべて stdio 上にあり、すべて JSON-RPC に隣接しています。同じエージェントが実行時にこの3つすべてに同時に参加します。

---

## なぜ HTTP ではなく Stdio なのか？

IDE 統合にとって、stdio にはいくつかの実用的な利点があります：

- **ゼロ設定**——選ぶポートも、TLS 証明書も、ファイアウォールルールも不要。
- **プロセススコープ**——ACP server は IDE セッションと運命を共にします。孤立したリスナーが残りません。
- **OS レベルの認証**——プロセスを起動できるなら、すでに必要な権限を持っています。API キーは不要。
- **トランスポート非依存**——同じ行プロトコルを SSH 越し、コンテナ内、VS Code remote セッション越しにトンネリングできます。

HTTP は Dashboard と Prometheus メトリクスのために依然利用可能ですが、IDE ↔ エージェントには stdio の方がシンプルで安全です。

---

## ストリーミングとマルチターン

Agent Card は `streaming: true` と `multi_turn: true` を宣言します。これはクライアントに次のことを伝えます：

- **ストリーミング**：長時間実行されるタスクは、単一のレスポンスだけでなく、同じ stdio 接続上で進捗イベントを発行できます。
- **マルチターン**：1つのタスクコンテキストが、状態を失うことなく複数のリクエスト／レスポンスのペア（明確化、フォローアップ）にまたがれます。

これらの能力は Session Memory Stack を反映します——ピン留めされた指示、スノーボール式のおさらい、キーファクトはすべて、チャネルメッセージと同じように、マルチターンの ACP 会話をまたいで引き継がれます。

---

## セキュリティ上の考慮事項

ACP は MCP と同様、DuDuClaw のセキュリティ境界を継承します：

- **CONTRACT.toml**——must_not/must_always ルールは依然適用されます。ACP 経由で送信されたタスクはこれらに違反できません。
- **能力ゲーティング**——`agent.toml [capabilities]` のデフォルト拒否は依然ツールアクセスをゲートします。
- **監査ログ**——ACP 経由で送信されたタスクは `audit.unified_log` に source=`acp` で出現します。
- **サンドボックス化**——タスクは依然 worktree レイヤーを、そして（オプションで）コンテナサンドボックスを通って実行されます。

クライアントが IDE であることが昇格された信頼を付与することはありません——エージェント自身のポリシーが最後の防衛線です。

---

## 他システムとの連携

- **Task Board**：ACP 経由で送信されたタスクは、チャネル経由で送信されたものと同じ `TaskStore` を流れます。両者とも Dashboard Activity Feed に表示されます。
- **Runtime 選択**：エージェントの通常の runtime（Claude/Codex/Gemini/OpenAI）が ACP タスクを処理します——同じセッションメモリ、同じ prompt cache 戦略、同じアカウントローテーション。
- **進化**：ACP タスクは、キーファクト抽出と予測エラー較正において「実質的なターン」としてカウントされます。
- **監査ログ**：すべての ACP リクエストは source=`acp` で記録され、他の4つの監査ソース（security / tool_calls / channel_failures / feedback）と並びます。

---

## なぜ重要か

### 標準ベースの統合

ACP は、実在するクライアント（Zed、nvim-acp、実験的な JetBrains プラグイン）を持つ実在のプロトコルです。これをサポートすることで、DuDuClaw は IDE ごとにカスタム統合を必要とするのではなく、成長中のエコシステムに加わります。

### 同じエージェント、新しいインターフェース

新しいエージェントも、新しい設定も、新しい runtime 境界もありません。既存のエージェント（SOUL.md、CONTRACT.toml、メモリ、スキル、wiki）が、新しいエントリポイントから単に到達可能になるだけです。エージェントの振る舞いへの投資はすべて引き継がれます。

### 開発者ループの加速

チャットアプリでエージェントに質問してからレスポンスをエディタにコピペするのではなく、開発者は作業する場所からエージェントを直接呼び出せます。摩擦はほぼゼロまで下がり、エージェントのレスポンスは*コンテキストの中に*着地します。

### 組み合わせ可能なオーケストレーション

1つのエージェントが別のエージェントの A2A クライアントになれます。オーケストレーター型のエージェントは、`agent/discover` でサブエージェントを探索し、そのスキルタグを確認し、`tasks/send` でタスクをルーティングできます——クロスプロセスのシナリオにおいて、DuDuClaw 内部のファイルベース IPC に対する構造化された標準的な代替手段です。

---

## まとめ

良いエージェントは、作業が起きる場所のどこからでも到達可能であるべきです。ダイニングルーム（チャネル）はエンドユーザーのためのもの。予約専用回線（ACP/A2A）は、プログラム的にそれと協働する必要のある IDE、パイプライン、ピアエージェントのためのものです。同じエージェント、同じ頭脳、同じ契約——ただ正面玄関により洗練されたプロトコルを備えただけです。
