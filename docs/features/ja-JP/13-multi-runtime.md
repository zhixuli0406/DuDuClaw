# マルチランタイムエージェント実行

> 1つのプラットフォーム、4つのAIバックエンド——Claude、Codex、Gemini、そしてOpenAI互換エンドポイント。

---

## たとえ話：多言語オフィス

翻訳者が必要なオフィスを想像してください。フランス語しか話せない翻訳者を1人雇う代わりに、翻訳デスクを構築します——フランス語、ドイツ語、日本語の翻訳者、またはクライアントの言語を話せるフリーランサーに仕事を割り当てられます。

翻訳デスクは*どの*翻訳者が仕事を処理するかは気にしません——翻訳の品質を気にします。フランス語翻訳者が忙しければ、次の対応可能な人にルーティングします。

DuDuClawのMulti-Runtimeアーキテクチャはその翻訳デスクです——ただしAIバックエンド用です。

---

## 仕組み

### AgentRuntime Trait

コアは全バックエンドが実装する統一インターフェース（`AgentRuntime`）です：

```
AgentRuntime trait:
  fn execute(prompt, tools, context) → Response
  fn stream(prompt, tools, context) → Stream<Event>
  fn health_check() → Status
```

すべてのバックエンド——Claude、Codex、Gemini、またはOpenAI互換エンドポイント——は同じインターフェースを実装します。システムの残りの部分は、どのバックエンドが特定のリクエストを処理しているか知る必要も、気にする必要もありません。

### 4つのバックエンド

**Claude Runtime** — Claude Code CLI（`claude`）をJSONLストリーミング出力で呼び出します。ネイティブMCPツールサポート、bash実行、Web検索、ファイル操作が組み込まれた最も機能豊富なバックエンドです。

```
Agent設定：runtime = "claude"
     |
     v
起動：claude --json --print ...
     |
     v
JSONLストリーミングイベントを解析
     |
     v
レスポンス + ツール呼び出しを抽出
```

**Codex Runtime** — OpenAI Codex CLIを`--json`フラグで呼び出し、構造化ストリーミングイベントを取得します。

```
Agent設定：runtime = "codex"
     |
     v
起動：codex --json ...
     |
     v
JSONL STDOUTイベントを解析
     |
     v
レスポンスを抽出
```

**Gemini Runtime** — Google Gemini CLIを`--output-format stream-json`で呼び出し、構造化出力を取得します。

```
Agent設定：runtime = "gemini"
     |
     v
起動：gemini --output-format stream-json ...
     |
     v
ストリーミングJSONイベントを解析
     |
     v
レスポンスを抽出
```

**OpenAI互換Runtime** — OpenAI chat completions APIを話す任意のHTTPエンドポイント（MiniMax、DeepSeek、ローカルサーバーなど）を呼び出します。

```
Agent設定：runtime = "openai-compat"
            api_url = "http://localhost:8080/v1"
     |
     v
HTTP POST /v1/chat/completions
     |
     v
SSEストリームを解析
     |
     v
レスポンスを抽出
```

### RuntimeRegistry：自動検出

DuDuClaw起動時、**RuntimeRegistry**はシステム上の利用可能なCLIツールをスキャンします：

```
起動スキャン：
     |
     v
  PATHに `claude` がある？ → Claude runtimeを登録
  PATHに `codex` がある？  → Codex runtimeを登録
  PATHに `gemini` がある？ → Gemini runtimeを登録
  設定済みHTTPエンドポイントがある？ → OpenAI互換runtimeを登録
     |
     v
Registryは利用可能なバックエンドを把握
```

エージェントは`agent.toml`で優先runtimeを指定できます：

```toml
[runtime]
preferred = "claude"    # プライマリバックエンド
fallback = "gemini"     # プライマリが利用不可時のフォールバック
```

### Per-Agent設定

異なるエージェントが異なるバックエンドを同時に使用できます：

```
Agent "dudu"（カスタマーサポート）→ Claude（最高の推論能力）
Agent "coder"（コード生成）    → Codex（コードに最適化）
Agent "analyst"（データ分析）   → Gemini（大規模コンテキストウィンドウ）
Agent "local"（プライバシー重視）→ OpenAI互換（ローカルエンドポイント）
```

---

## クロスプロバイダーフェイルオーバー

バックエンドが利用不可になった場合（レート制限、ダウン、エラー）、**FailoverManager**が自動的に次の利用可能なバックエンドに切り替えます：

```
Claude runtime：レート制限中（クールダウン：2分）
     |
     v
FailoverManagerがagent設定を確認：
  fallback = "gemini"
     |
     v
Gemini runtimeにルーティング
     |
     v
Claudeクールダウン完了 → プライマリルーティングを復元
```

フェイルオーバーはユーザーに透過的です——どのバックエンドが処理しても、ユーザーはレスポンスを受け取ります。

---

## なぜ重要か

### ベンダーロックインなし

DuDuClawは単一AIプロバイダーに賭けません。Claudeが値上げすれば、CodexやGeminiにエージェントを移行できます。

### 各タスクに最適なツール

コード生成はCodexの方が効果的かもしれません。複雑な推論はClaudeの方が強いかもしれません。Multi-Runtimeにより、正しいタスクに正しい頭脳をマッチングできます。

### レジリエンス

1つのプロバイダーがダウンしても、他が稼働し続けます。ローカル推論フォールバックと組み合わせることで、DuDuClawはどの単一プロバイダーの障害にも耐えられます。

---

## 他システムとの連携

- **Account Rotator**：全プロバイダーの認証情報を管理、クロスプロバイダーフェイルオーバー付き。
- **Confidence Router**：runtimeレイヤーの下位に位置——ローカル vs. クラウドを決定。Runtimeレイヤーは*どの*クラウドかを決定。
- **CostTelemetry**：プロバイダーごとのコストを追跡し、情報に基づくルーティング決定を支援。
- **MCP Server**：ツールはサポートする全バックエンドに公開（ClaudeはネイティブMCP経由、その他はツールインジェクション経由）。

---

## まとめ

AI領域はマルチプロバイダーです。単一CLIの上に構築するのは、単一OSのためだけにソフトウェアを書くようなもの——動くけど、いつか動かなくなります。`AgentRuntime` traitが差異を抽象化し、DuDuClawがClaude、Codex、Gemini、そしてOpenAI互換エンドポイントを交換可能なバックエンドとして扱えるようにします。
