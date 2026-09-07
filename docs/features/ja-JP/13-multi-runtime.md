# マルチランタイムエージェント実行

> 1つのプラットフォーム、12のAIバックエンド——Claude、Codex、Gemini、Antigravity、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode、そしてOpenAI互換エンドポイント。

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

### ランタイムカタログ

各バックエンドの記述は 1 か所だけ、単一のコンパイル時テーブル（`crates/duduclaw-core/src/runtime_catalog.rs`）にあります。検出、ワンクリック インストール、モデル探索、CLI ログイン、モデル↔プロバイダー推論はすべてこのテーブルを読みます——「インストールできるのに検出されない」「設定できるのにログインできない」ランタイムは構造的に発生しません。

| ランタイム | バイナリ | インストール経路 | ヘッドレス呼び出し | 出力 | ログイン | 認証情報の保存先 |
|---|---|---|---|---|---|---|
| Claude Code | `claude` | npm `@anthropic-ai/claude-code` | `-p <prompt> --output-format stream-json` | jsonl | `claude setup-token`（コード貼り付け） | `~/.claude/.credentials.json` |
| OpenAI Codex | `codex` | npm `@openai/codex` | `exec --json <prompt>` | jsonl | `codex login`（localhost コールバック） | `~/.codex/auth.json` |
| Gemini CLI | `gemini` | npm `@google/gemini-cli` | `-p --output-format stream-json <prompt>` | jsonl | `gemini auth login`（localhost コールバック） | `~/.gemini/oauth_creds.json` |
| Google Antigravity | `agy` | `antigravity.google/cli/install.sh` | `-p <prompt>` | text | `agy login`（localhost コールバック） | — |
| Grok Build | `grok` | `x.ai/cli/install.sh`（手動） | `-p <prompt>` | text | `grok login --device-code` | `~/.grok/auth.json` |
| Qwen Code | `qwen` | npm `@qwen-code/qwen-code` | `-p <prompt> --yolo --output-format json` | json | なし（API キーのみ） | `~/.qwen/.env` |
| Kimi Code | `kimi` | npm `@moonshot-ai/kimi-code` | `-p <prompt> --output-format stream-json` | jsonl | `kimi login`（デバイスコード） | `~/.kimi-code/credentials/` |
| GitHub Copilot CLI | `copilot` | npm `@github/copilot` | `-p <prompt> -s --no-ask-user --allow-all-tools` | text | `copilot login --device-code` | `~/.copilot/config.json` |
| Kiro CLI | `kiro-cli` | `cli.kiro.dev/install`（手動） | `chat --no-interactive --trust-all-tools <prompt>` | text | `kiro-cli login --use-device-flow` | `~/.kiro/settings/cli.json` |
| Cursor CLI | `cursor-agent` | `cursor.com/install` | `-p <prompt> --force --output-format json` | json | `cursor-agent login`（ブラウザ） | `~/.cursor/cli-config.json` |
| Mistral Vibe | `vibe` | PyPI `mistral-vibe` | `-p <prompt> --yolo --trust --output json` | json | なし（API キーのみ） | `~/.vibe/.env` |
| OpenCode | `opencode` | `opencode.ai/install` | `run <prompt> --auto --format json` | jsonl | `opencode auth login` | `~/.local/share/opencode/auth.json` |
| OpenAI 互換エンドポイント | *(HTTP)* | — | — | json | なし（API キーのみ） | — |

モデル指定の書き方はベンダーごとに異なり、カタログはどの形式かも記録します：独立したフラグ（`--model <id>`）、Copilot が文書化している等号形式（`--model=<id>`）、フラグを持たない CLI 向けの環境変数（Mistral Vibe の `VIBE_ACTIVE_MODEL`）、あるいは無し（Kiro は呼び出しごとではなく `kiro-cli settings` でモデルを選びます）。

**ランタイムを有効化する前に読むべきベンダー規約：**

- **Kiro**——AWS の FAQ には、Kiro のネイティブ インターフェース以外へリクエストを流すサードパーティ製自動化ハーネス経由の利用は許可されないと明記されています。DuDuClaw から Kiro を駆動することはこれに該当します。自身の CI から `kiro-cli` を直接呼び出すことは許可されています。Kiro のインストール経路が手動なのはこのためです——判断はあなたが明示的に行ってください。
- **Anthropic / Google**——2026-03 以降、サードパーティ製品による消費者向けサブスクリプション トークンの利用はサーバー側でブロックされ、アカウント停止の事例があります。API キーをご利用ください。
- **Qwen**——無料 OAuth ティアは 2026-04-15 に終了しました。API キー（ModelStudio / DashScope）のみです。
- **OpenCode**——MIT で独自の制限はありませんが、上記の理由により 1.3.0 で Anthropic サブスクリプション プラグインを削除しました。プロバイダーの API キーをご利用ください。
- **OpenAI**——サードパーティ製品が ChatGPT サブスクリプションのログインを利用することについての方針は不明です。API キーがサポートされる経路です。

ダッシュボードは該当する注意事項を表示し、サブスクリプション ログインを開始する前に「リスクを理解しました」の明示的な同意を求めます。

### バックエンドの駆動方法

5 つのバックエンドは専用のランタイム モジュールを持ちます。共有できないベンダー固有の配線——アカウント ローテーション（Claude）、各 CLI 独自形式での MCP 設定注入、ケーパビリティ→サンドボックス フラグの変換、出力が空だった場合の PTY リカバリ——があるためです。それ以外はすべて、カタログ エントリーからそのまま組み立てられる**単一の**汎用 print-mode ランタイム（`runtime/generic_cli.rs`）が駆動します：テンプレート argv でバイナリを起動し、プロンプトを引数または stdin で渡し、text / JSON / JSONL を最終応答テキストへ解析し、非ゼロ終了や認証要求のマーカーをフェイルオーバー チェーンが理解できる型付きエラーへ対応付けます。

### 当初の 4 つのバックエンド

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
起動スキャン（ランタイムカタログを1周するループ）：
     |
     v
  バイナリを持つカタログエントリーごとに：
     PATH → ~/.local/bin、Homebrew、bun/volta/npm-global/asdf shim、
     /opt/duduclaw/runtimes/bin、/usr/bin、/bin
       見つかった？ → 登録（専用モジュールがあればそれ、なければ汎用 print-mode）
     |
     v
  常に：OpenAI互換（バイナリではなくAPIキーで判定するHTTPエンドポイント）
     |
     v
Registryは利用可能なバックエンドを把握
```

`/opt/duduclaw/runtimes/bin` は DuDuClaw OS アプライアンス イメージが同梱 CLI を配置する場所です。ゲートウェイが対話的な `PATH` を引き継いでいなくても、イメージ同梱のランタイムは検出されます。

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

## プロバイダー対応アカウント（WP-A、2026-09）

`accounts.add`——ダッシュボードのアカウントページと OOBE の「AI Runtime 認可」ステップの両方が使う gateway RPC——は、既存の `type`（`api_key` | `oauth`）に加えて `provider` id を受け付けるようになりました。受け付けられる id はプラットフォーム共通の provider 対照表（`duduclaw_core::provider_env::KNOWN_PROVIDER_IDS`）に列挙されたもの——`anthropic`、`openai`、`gemini`/`google`、`deepseek`、`minimax`、`groq`、`together`、`mistral`、`openrouter`、`xai`、`qwen`——です。`provider` を省略すると `"anthropic"` がデフォルトになるため、この機能より前に書かれた呼び出し元は一字一句変わらず動作します。未知の id は黙って受理されず、拒否されます。

認証情報は引き続き `config.toml` の `[[accounts]]` 配列に書き込まれますが、provider が付与されるようになりました。

```toml
[[accounts]]
id = "openai-prod"
type = "api_key"
provider = "openai"
api_key_enc = "..."          # anthropic は従来通り anthropic_api_key_enc フィールドを使う
```

`AccountRotator::select_for_provider`——Claude CLI パスとクロスプロバイダー Direct-API の `duduclaw-llm` provider パスが以前から使っていたのと同じ選択ロジック——がこのフィールドで厳密にフィルタするため、この方法で追加された OpenAI／Gemini／xAI／DeepSeek…のキーは、Anthropic アカウントとまったく同じローテーション・予算管理・クールダウンの仕組みに乗ります。読み取り側で provider ごとのコードパスを新設する必要はありませんでした。`accounts.list` と `accounts.budget_summary` はどちらも各アカウントの `provider` を返すようになり、ダッシュボードのアカウントページでどのベンダーのキーかが分かるようになりました。`AddAccountDialog` にはプロバイダー選択と、各プロバイダーのキー形式のヒント、ベンダー自身のコンソールで API キーを取得するリンクが追加されています。

## サブスクリプションログインのリスク開示

「サブスクリプションでワンクリックログイン」する全フロー——CLI ログインモーダル、案内付き QR コード設定ウィザード、そしてそのどちらかを開くだけの OOBE の runtime 設定カード——は、開始前に必ずリスク通知を表示します。Anthropic と Google は 2026 年 3 月以降、サードパーティ製品による消費者向けサブスクリプショントークンの利用をサーバー側でブロックしており、実際にアカウントが停止された例もあります。OpenAI の方針は現時点で不明です。「リスクを理解し、自己責任で同意します」というチェックボックスに同意しない限り、フロー自体のログインステップ（CLI サブプロセス、ブラウザコールバック、デバイスコードのポーリング）は開始されません。API キーのパスはこのゲートの影響を受けず、引き続き推奨されるデフォルトです。

---

## まとめ

AI領域はマルチプロバイダーです。単一CLIの上に構築するのは、単一OSのためだけにソフトウェアを書くようなもの——動くけど、いつか動かなくなります。`AgentRuntime` traitが差異を抽象化し、DuDuClawがClaude、Codex、Gemini、そしてOpenAI互換エンドポイントを交換可能なバックエンドとして扱えるようにします。
