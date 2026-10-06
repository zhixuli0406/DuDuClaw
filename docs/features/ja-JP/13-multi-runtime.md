# マルチランタイムエージェント実行

> 1つのプラットフォームに 13 のランタイム ID：12 の CLI バックエンド（Claude、Codex、Gemini、Antigravity、Grok、Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode）と、任意の OpenAI 互換エンドポイントに HTTP で接続する `openai_compat` です。

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
| Gemini CLI（v1.67.0 で非推奨、v1.71.0 で削除） | `gemini` | npm `@google/gemini-cli` | `-p --output-format stream-json <prompt>` | jsonl | `gemini auth login`（localhost コールバック） | `~/.gemini/oauth_creds.json` |
| Google Antigravity | `agy` | `antigravity.google/cli/install.sh` | `-p <prompt>` | stream-json（v1.2.10） | ターミナルで `agy` を実行して Google サインイン（`login` サブコマンドなし）、または API キーモード | OS keyring |
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

5 つの CLI バックエンド（`claude`、`codex`、`gemini`、`antigravity`、`grok`。`runtime/mod.rs` の `BESPOKE_RUNTIME_IDS`）は専用のランタイム モジュールを持ちます。共有できないベンダー固有の配線——アカウント ローテーション（Claude）、各 CLI 独自形式での MCP 設定注入、ケーパビリティ→サンドボックス フラグの変換、出力が空だった場合の PTY リカバリ——があるためです。それ以外はすべて、カタログ エントリーからそのまま組み立てられる**単一の**汎用 print-mode ランタイム（`runtime/generic_cli.rs`）が駆動します：テンプレート argv でバイナリを起動し、プロンプトを引数または stdin で渡し、text / JSON / JSONL を最終応答テキストへ解析し、非ゼロ終了や認証要求のマーカーをフェイルオーバー チェーンが理解できる型付きエラーへ対応付けます。残り 7 つの CLI（Qwen Code、Kimi Code、GitHub Copilot CLI、Kiro、Cursor、Mistral Vibe、OpenCode）はこの汎用ランタイムを通ります。`openai_compat` は CLI ではなく、独自の HTTP モジュール（`runtime/openai_compat.rs`）を持ちます。手書きのモジュールは合計 6 つです。

### 当初の 4 つのバックエンド

以下は DuDuClaw が最初に出荷した 4 つのバックエンドです。専用モジュールを持つ残り 2 つの CLI、Antigravity と Grok は、後の各セクションで説明します。

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

> **v1.67.0 で非推奨、v1.71.0 で削除予定。** Google は 2026-06-18 に、個人アカウント（無料、AI Pro、AI Ultra）に対する Gemini CLI での提供を停止しました。Antigravity ランタイムをご利用ください。削除までは従来どおり動作します。Gemini API プロバイダーは影響を受けません。移行手順は[非推奨となった名称](../../guides/ja-JP/deprecations.md#gemini-cli-ランタイム)を参照してください。

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

エージェントは`agent.toml`で使用するruntimeを指定します：

```toml
[runtime]
provider = "claude"          # プライマリバックエンド
fallback = "antigravity"     # プライマリが利用不可時のフォールバック
```

`provider` がない場合、エージェントは Claude で動作します。認識できない値は警告を記録し、同じく Claude にフォールバックします。

### Per-Agent設定

異なるエージェントが異なるバックエンドを同時に使用できます：

```
Agent "dudu"（カスタマーサポート）→ Claude（最高の推論能力）
Agent "coder"（コード生成）    → Codex（コードに最適化）
Agent "analyst"（データ分析）   → Antigravity
Agent "local"（プライバシー重視）→ OpenAI互換（ローカルエンドポイント）
```

つまり、単一のDuDuClawインストールで複数のAIプロバイダーにまたがるエージェントを統括でき、それぞれがタスクに最適なバックエンドを使えます。

---

## Effort

最近の推論モデルには、モデルの選択とは別に**深さ**のダイヤルがあります。この 1 回の呼び出しでどれだけ深く考えるか、という設定です。ベンダーごとに書き方が異なり、受け付ける値も同じではありません。DuDuClaw はこれを 1 つの設定にまとめ、変換を引き受けます。

エージェントに設定します。

```toml
# <home>/agents/<id>/agent.toml
[model]
preferred = "claude-opus-5"
effort    = "high"          # low | medium | high | xhigh | max
```

未設定がデフォルトで、*フラグを一切渡さない*ことを意味します。spawn の内容はこの機能のない DuDuClaw とバイト単位で同一で、プロバイダー自身のデフォルトの深さが適用されます。

### ランタイムごとのフラグ対応

インストール済みのバイナリに対して 2026-09-24 に実測した結果です（`research/multi-model-routing-2026-09/17-P0-cli-flag-probe.md` §4 + §6）。ドキュメントからの推測ではありません。

| ランタイム | 実測バージョン | effort の表現方法 | CLI が受け付ける値 |
|---|---|---|---|
| `claude` | 2.1.258 | `--effort <v>` | `low` `medium` `high` `xhigh` `max` |
| `codex` | 0.156.1 | `-c model_reasoning_effort=<v>`（設定オーバーライド、専用フラグなし） | `low` `medium` `high` `xhigh` |
| `antigravity`（`agy`） | 1.2.10 | `--effort <v>` | `low` `medium` `high` |
| `grok` | 1.0.41 | `--reasoning-effort <v>`（別名 `--effort`） | *`--help` では列挙されていません* |
| `gemini` | — | **フラグが存在しません**。debug ログに記録して無視します | — |
| `openai_compat` | — | リクエストボディの `reasoning_effort` | `low` `medium` `high` |

### クランプ表

受け付ける値の集合が異なるため、設定値は対象ランタイムが受け付ける値まで**下方向に**クランプされます。黙って捨てられることも、CLI が拒否する値が送られることもありません。

| 設定値 | claude | codex | antigravity | grok | openai_compat | gemini |
|---|---|---|---|---|---|---|
| `low` | `low` | `low` | `low` | `low` | `low` | — |
| `medium` | `medium` | `medium` | `medium` | `medium` | `medium` | — |
| `high` | `high` | `high` | `high` | `high` | `high` | — |
| `xhigh` | `xhigh` | `xhigh` | **`high`** | **`high`** | **`high`** | — |
| `max` | `max` | **`xhigh`** | **`high`** | **`high`** | **`high`** | — |

Grok を意図的に `high` で頭打ちにしているのは、`--help` がフラグ名には触れていても値を列挙していないためです。`xhigh`/`max` を渡すと "unexpected value" で spawn 全体が落ちるおそれがあります。`openai_compat` も、8 つの異質なプリセットにまたがるため同じ理由で上限を設けています。どちらの上限も 1 か所（`duduclaw-core/src/effort.rs`）にあり、実機での動作確認が取れた時点で引き上げられます。

### Direct-API の対応

API レベルの経路（`duduclaw-llm`）は、同じ値を各ベンダーのネイティブなフィールドへ載せます。

| プロトコル | フィールド |
|---|---|
| Anthropic Messages | `output_config.effort`（GA、beta ヘッダー不要） |
| OpenAI Responses | `reasoning.effort` |
| OpenAI-compat chat/completions | `reasoning_effort`（トップレベル） |
| Gemini `generateContent` | `generationConfig.thinkingConfig.thinkingLevel`。**未検証**、下記参照 |

> Gemini のキーは、**確認が取れていない**唯一の対応です。入れ物としての `thinkingConfig` は検証済みで（既存の `thinkingBudget` が使っていて、すでに出荷されています）、同じ階層の `thinkingLevel` キーは確認できませんでした。ai.google.dev を 2 回取得しましたが、どちらも `GenerationConfig` のリファレンスが途中で切れていて、このキーに触れていませんでした。Interactions API では `generation_config.thinking_level` と書かれているため、`generateContent` 側の camelCase 版は推測です。フィールドが設定されたときだけ送られるので、effort が未設定ならこのキーが送られることはありません。頼る前に再検証してください。

### コストとキャッシュ

引き上げる前に知っておくことが 2 つあります。

- **effort はトークンを消費します。** キャッシュやプロンプトの整理といった無料の改善の次に来る、品質とコストを交換する最初のレバーで、範囲の最上位は本当に難しい作業でだけ元が取れます。コーディングや長時間のエージェントタスクでは効果が大きく、チャット、分類、大量処理のルートでは `low` で十分なことが多くあります。
- **会話の途中で effort を変えると、多くのモデルでプロンプトキャッシュが無効になります。** effort はキャッシュされるプレフィックスの一部になるためです。エージェントごとに値を 1 つ決めたらそのままにし、ターンごとに調整しないでください。

effort を意図的にエージェント主導にしていない場所が 1 つあります。軽量な抽出経路（セッション圧縮、GVU、wiki 取り込み）で、`medium` に固定されています。機械的な抽出が、会話エージェントを `max` に上げたせいで高くなるべきではないからです。

### PTY プール

*（2026-09 に削除。）* 以前、effort は PTY プールの**セッションキャッシュキー**の一部で、異なる effort を求める 2 つの呼び出しには、別々のプール済みセッションが割り当てられていました。プールはなくなり、すべての spawn が自分自身の `--effort` フラグを持ちます。

---

## クロスプロバイダーフェイルオーバー

バックエンドが利用不可になった場合（レート制限、ダウン、エラー）、**FailoverManager**が自動的に次の利用可能なバックエンドに切り替えます：

```
Claude runtime：レート制限中（クールダウン：2分）
     |
     v
FailoverManagerがagent設定を確認：
  fallback = "antigravity"
     |
     v
Antigravity runtimeにルーティング
     |
     v
Claudeクールダウン完了 → プライマリルーティングを復元
```

フェイルオーバーはユーザーに透過的です——どのバックエンドが処理しても、ユーザーはレスポンスを受け取ります。ヘルス状態はバックエンドごとに独立して追跡されます：

- **Healthy**：通常動作
- **Rate-Limited**：短いクールダウン（2分）
- **Error**：指数バックオフ
- **Non-Retryable**：手動対応が必要（認証失敗、課金）

---

## なぜ重要か

### ベンダーロックインなし

DuDuClawは単一AIプロバイダーに賭けません。Claudeが値上げすれば、CodexやGeminiにエージェントを移行できます。Geminiが強力な新機能を追加すれば、インフラを作り直さずに採用できます。

### 各タスクに最適なツール

コード生成はCodexの方が効果的かもしれません。複雑な推論はClaudeの方が強いかもしれません。データ分析はGeminiの大きなコンテキストウィンドウの恩恵を受けるかもしれません。Multi-Runtimeにより、正しいタスクに正しい頭脳をマッチングできます。

### レジリエンス

1つのプロバイダーがダウンしても、他が稼働し続けます。ローカル推論フォールバックと組み合わせることで、DuDuClawはどの単一プロバイダーの障害にも耐えられます。

### コスト最適化

プロバイダーごとに料金が異なります。`LeastCost`ローテーション戦略は、クエリの種類ごとに最も価格性能比の高いプロバイダーへルーティングできます。

---

## 他システムとの連携

### Codex の非対話承認（2026-09）

Codex 0.156.x は、すべての MCP ツール呼び出しを承認リクエストの背後に置きます。`approval_policy=never` ではそのリクエストが自動拒否され、`mcp_servers.<id>.default_tools_approval_mode` も `projects.<cwd>.trust_level` も結果を変えません。`--approve-for-me`（自動レビュー）がサポートされている非対話の抜け道で、`-s/--sandbox` とは同時に指定できません。エージェントのディレクトリは git リポジトリではないため、`--skip-git-repo-check` を常に渡し、stdin を閉じます。

**ケーパビリティのレベルごとに 1 組のフラグ**（2026-09-28 に変更。エージェントを制限する前に ReadOnly の行を読んでください）：

| `[capabilities]` のレベル | Codex のフラグ | エージェントにできること |
|---|---|---|
| ReadOnly（書き込みツールを付与していない、またはすべて拒否） | `-s read-only -c approval_policy=never` | 読み取りと推論ができます。書き込みは**実際にブロック**されます。**すべての MCP ツール呼び出しが自動拒否**されるため、その回のエージェントは duduclaw のツールを持ちません。spawn ごとに `warn!` を 1 件出力して知らせます。 |
| WorkspaceWrite（デフォルト） | `--approve-for-me -c approval_policy=never -c sandbox_mode="workspace-write"` | ワークスペース内に書き込めます。duduclaw の MCP ツールをすべて使えます。 |
| FullAccess（明示的な `computer_use = true`） | `--dangerously-bypass-approvals-and-sandbox` | 制限なし。オペレーターが明示的に付与した場合のみです。 |

2026-09-28 までは、ReadOnly でも `--approve-for-me` と `-c sandbox_mode="read-only"` を使っていました。これは**フェイルオープン**でした。`--approve-for-me` の自動レビューは workspace-write のサンドボックスで動くため、read-only の宣言は形だけのもので、ケーパビリティを制限したエージェントでもファイルを書けてしまいました。現在は実際に効力のあるフラグを渡しており、その代償が MCP ツールの面です。エージェントにツールを残したい場合は WorkspaceWrite を付与してください。Codex における ReadOnly は「何も変更してはならない」という意味で、ツールもそこに含まれます。

### Codex の MCP 認証情報：0.157 以降は `env_vars`、それより前は `argv` にフォールバック（2026-09-28）

Codex の spawn は、呼び出しごとの `-c` 設定オーバーライドで duduclaw の MCP サーバーを登録します。その登録が運ぶ値のうち 2 つは機密です。`DUDUCLAW_MCP_API_KEY` と `DUDUCLAW_AGENT_TOKEN` です。この登録（と以下の処理すべて）は ReadOnly を含むすべての Codex spawn で行われます。ReadOnly ではサーバーは登録されますが、上の表のとおり、Codex がそのサーバーへの呼び出しをすべて自動拒否します。

**認証情報を環境変数に置くだけでは済まない理由。** 2026-09-28 に実機で確認し、Codex のソースとも突き合わせました。Codex はすべての stdio MCP サーバー子プロセスに `env_clear()` をかけ、11 個の名前のデフォルト許可リスト（`HOME`、`PATH`、`SHELL`、`USER`、`LOGNAME`、`TERM`、`TMPDIR`、`TZ`、`LANG`、`LC_ALL`、`__CF_USER_TEXT_ENCODING`）と、設定で宣言されたものだけを戻します。gateway 自身のプロセス環境は MCP サーバーに届かないため、`Command::env()` だけでは何も渡りません。設定チャネルが唯一のチャネルです。

**DuDuClaw の現在の動作。** 設定チャネルには 2 つの形があり、その Codex バイナリが報告するバージョンから、バイナリごとに決めます。

| Codex のバージョン | 認証情報の形 | `ps` に見えるもの |
|---|---|---|
| **≥ 0.157.0** | `-c mcp_servers.duduclaw.env_vars=["DUDUCLAW_MCP_API_KEY", "DUDUCLAW_AGENT_TOKEN"]`。値は Codex の**プロセス**環境に設定し、Codex がそこから MCP 子プロセスへコピーします | 変数の**名前**のみ |
| **< 0.157.0**、またはバージョンを読み取れない | `-c mcp_servers.duduclaw.env.<K>="<value>"`（従来の動作） | 認証情報の**値** |

認証情報ではない項目（`DUDUCLAW_HOME`、`DUDUCLAW_PORT`、`DUDUCLAW_AGENT_ID`、`DUDUCLAW_INSTANCE`）は、どちらの経路でも `env.<K>="<value>"` の形のままです。機密ではなく、設定テーブルに残しておけば、プロセス環境が将来消去されても登録は機能するからです。「認証情報」かどうかは名前の末尾の完全一致で決めます。`_API_KEY`、`_TOKEN`、`_SECRET`、`_PASSWORD`（ASCII の大文字小文字を区別しない）で、`duduclaw-core` の spawn-env 許可リストが強制するのと同じ形の規約です。

**無条件ではなくバージョンで切り替える理由。** `env_vars` は `codex-cli 0.157.1` で動作を確認済みですが、このキーを受け付ける最小バージョンは未確認で、`RawMcpServerConfig` には `deny_unknown_fields` が付いています。知らないほど古い Codex では、設定の解析時に実行が落ちる（すべての spawn が失われる）か、認証情報が黙って捨てられる（エラーなしにエージェントが duduclaw のツールをすべて失う）かのどちらかです。そこで gateway はバイナリのパスごと、プロセスごとに 1 回だけ `codex --version` を実行し、`codex-cli X.Y.Z` を解析して、`0.157.0` 未満、解析不能、探査不能（spawn 失敗、非ゼロ終了、5 秒のタイムアウト）のいずれも「非対応」として扱い、`warn!` を 1 件出して従来の `argv` の形へフォールバックします。探査の失敗が spawn を失敗させることはありません。

**フォールバック経路にいる場合**（古い Codex、共有またはマルチテナントのホスト）は、露出が現実のものになります。コマンドライン引数は同じホスト上のどのプロセスからも読めます（`ps -ww`、`/proc/<pid>/cmdline`）。Codex CLI を 0.157.1 以降にアップグレードすれば、設定を変えなくても認証情報が `argv` から外れます。

**両方の経路に共通する緩和策。** `argv` に載せてよい env キーの集合は、既知の `DUDUCLAW_*` のブロックにテストで固定されており、新しい機密が黙って加わることはありません。すべてのキーは、展開前に素の TOML キーであることを検証され（`env_vars` 配列内の名前も含む）、すべての値は TOML のクォートが施されます。

### 作業ディレクトリのオーバーライドはすべての CLI バックエンドに届く（2026-09-28）

呼び出し側は、ある spawn をエージェント自身のディレクトリ以外の場所で実行するよう求められます。現在の呼び出し側はチームコンポーザー（team composer）だけで、ロールメンバーを従業員のワークスペースに置きます。こうすると、使い捨ての scaffold がメンバーの終了と同時にガベージコレクトされても、メンバーが書いたファイルは残ります。

2026-09-28 までは、このリクエストに応えるのは Codex バックエンドだけでした。Gemini、Antigravity、Grok は、依頼にかかわらずエージェントのディレクトリで spawn されたため、この 3 つのいずれかで動くロールメンバーは、数秒後に削除されるディレクトリで作業していました。現在は 4 つすべてが共通のヘルパーで作業ルートを解決し、要求されたパスが実在するディレクトリかどうかも確認します。そうでなければ警告を出し、どこでもない場所へ spawn せずにエージェントのディレクトリへフォールバックします。ネイティブな OS サンドボックスも同じルートを対象にするため、書き込み権限を得るのはオーバーライドされたルートです。

オーバーライドが動かすのは**作業ディレクトリだけ**です。エージェントの ID（MCP サーバーの登録、ツールが認証に使うエージェント ID、エージェント自身の設定）はエージェントのディレクトリに残ります。

バックエンド固有の影響が 2 つあります。見つけてもらうのではなく、先に書いておきます。

- **Antigravity** は作業ルートを事前に信頼済みにし（`agy` は対話的な「このワークスペースを信頼しますか？」のプロンプトを出し、ヘッドレス実行を止めてしまうため）、`--add-dir` として渡します。
- **Grok** は、MCP 登録（`.grok/config.toml`）とサンドボックスプロファイル名（`.grok/sandbox.toml`）の両方を、エージェントのディレクトリではなく作業ディレクトリから解決します。そのためオーバーライドされたルートには両ファイルのコピーが置かれ、そうしなければメンバーはツールなし、解決できないサンドボックスプロファイルで spawn されます。既知の制限：この 2 つのファイルはディレクトリをキーにしているため、1 つのワークスペースを共有する 2 つの Grok ロールメンバーは、互いに宣言済みの env ブロックを上書きします。command/args の部分はメンバー間で同一で、MCP 子プロセスが実際に認証に使うのはプロセスごとの ID なので、影響範囲は宣言されたブロックだけです。

### Antigravity の認証と MCP ツール（2026-10-01）

`agy` には `login` サブコマンドがないため、ダッシュボードにはワンクリックのサインインがありません。認証方法は 2 つです。

- **Google サインイン**：DuDuClaw が動いているホストのターミナルで `agy` を実行し、案内に従います。認証情報は OS の keyring に保存されるため、keyring やブラウザのないコンテナ・リモートホストでは使えません。
- **API キーモード**：`config.toml` に `[antigravity] auth = "api_key"` を設定し、Gemini API キーを `gemini` プロバイダーのアカウント、または環境変数 `GEMINI_API_KEY` として用意します。Gateway が agy の設定に `modelProvider` を自動で書き込みます。`auth = "login"` で Google サインインに戻り、その `modelProvider` の項目も削除されます。`auth` を一度も設定していない場合、gateway は `modelProvider` に触れず、Gemini キーも渡さないため、agy はそれまでの認証方法のままです。

`ANTIGRAVITY_API_KEY` という変数は存在しません。プラットフォームの MCP ツールは、各エージェントのワークスペースの `<agent workspace>/.agents/mcp_config.json` に登録されます。

API key モードに切り替える前に知っておくこと：

- この設定は OS ユーザー全体に効きます。agy は `modelProvider` をユーザーレベルの設定ファイルに保存するため、同じアカウントで対話的に使う `agy` も API key 経路に切り替わります。
- 同じ OS ユーザーで 2 つの gateway を動かし、`auth` に異なる値を設定すると、互いにこの項目を上書きします。
- `api_key` を使った後に Google サインインへ戻すには、`auth = "login"` を明示してください。`auth` の行を削除するだけでは戻りません。設定がない場合 gateway は `modelProvider` に触れないため、以前に書き込まれた `"gemini"` が agy の設定に残り、gateway はログで通知するだけです。`login` モードでは、gateway は `GEMINI_API_KEY`／`GOOGLE_API_KEY` を agy とその実行コマンドに渡しません。`api_key` モードでは、エージェントの shell からこのキーを読めます（agy は環境変数から受け取る必要があるため）。

### Antigravity のツール権限（v1.69.1）

`agy` 1.2.16 は print mode で、人間に尋ねられない確認をすべて自動で拒否します。MCP ツールの呼び出しにはその確認が 1 回必要です。この修正の前は、既定のケーパビリティレベル（`--sandbox` 付き）の Antigravity 従業員はプラットフォームのツールを一切使えず、`--dangerously-skip-permissions` を渡すフルアクセスのレベルだけが使えました。この不具合は v1.67.0 からあり、2026-10-04 に実際の Gemini API キーで検証して初めて見つかりました。

Gateway は Antigravity のターンを実行するたびに、作業ルートを agy のユーザーレベル設定ファイル `~/.gemini/antigravity-cli/settings.json` の `trustedWorkspaces` へすでに追加しています。同じロック付き書き込みで、`permissions.allow` に次の 2 つのルールも追加するようになりました。

- `mcp(duduclaw/*)` は、`duduclaw` という名前で登録された MCP サーバーのすべてのツールを許可します。
- `read_file(<HOME>/.gemini/antigravity-cli/mcp/duduclaw)` は、そのサーバーのツール説明ファイルの読み取りを許可します。agy の MCP ツールは遅延ロードで、モデルは呼び出しの前に毎回説明ファイルを読みます。この読み取りも print mode では拒否されます。`HOME` の正規化後のパスが異なる場合（たとえば macOS の `/var` と `/private/var`）は、両方の表記を書き込みます。

Gateway はシェルコマンド、ファイル書き込み、URL のルールを追加せず、コマンドラインのフラグも変えていないため、既定のレベルは引き続き `--sandbox` で動きます。agy 1.2.16 と実際の Gemini API キーでのテストでは、既定のレベルの従業員は DuDuClaw のツールを呼び出せました。同じターンのシェルコマンドと作業領域外へのファイル書き込みは引き続き拒否され、パストラバーサルによる読み取りと、ディレクトリ外を指すシンボリックリンク経由の読み取りも拒否されました。`--sandbox` と「すべて自動承認」のフラグを併用する方法は、ファイル書き込みツールが作業領域外に書き込めてしまうため採用していません。

オペレーターのルールは保持されます。既存の `allow`、`deny`、`ask` の項目はそのままです。`permissions` がオブジェクトでない、または `allow` が配列でない場合、gateway はそれを書き換えず警告を記録します。ワークスペースの信頼と `modelProvider` は書き込まれ、実行が失敗したときのエラーにはルールを追加できなかったことが書かれます。HOME のパスに `(`、`)`、`,`、`*`、改行が含まれる場合、または有効な UTF-8 でない場合は、`read_file` ルールを省略して警告を記録し、`mcp(duduclaw/*)` だけを書き込みます。

**読み取り専用レベル。** これらのルールはユーザーレベルのファイルにあるため、従業員のケーパビリティレベルで切り替えられません。そのため、読み取り専用の Antigravity 従業員にも同じ 2 つのルールが適用され、プラットフォームのツールを呼び出せます。何ができるかは、MCP サーバー側の `allowed_tools`、`denied_tools`、承認リストが決めます。これは Claude ランタイムと同じで、Codex とは異なります。Codex は読み取り専用レベルですべての MCP ツール呼び出しが拒否されます（上の表を参照）。

**知っておくべき副作用。**

- ルールは、その OS ユーザーのすべての `agy` が共有する設定ファイルに書かれます。ターミナルで自分で `agy` を対話的に使うときも、`duduclaw` という名前の MCP サーバーのツール呼び出しと、その説明ファイルのディレクトリの読み取りは、確認なしで自動的に許可されます。
- ルールは追加されるだけで、削除はされません。従業員を削除した後、DuDuClaw をアンインストールした後、Antigravity を使わなくなった後も、ルールはファイルに残ります（`trustedWorkspaces` の項目も元からそうです）。削除するには `~/.gemini/antigravity-cli/settings.json` を編集し、`permissions.allow` からこの 2 つを消します。
- 同じ OS ユーザーで gateway を 2 つ動かしても、両者は同じルールを書くため、異なる内容で上書きし合うことはありません。

**エラーメッセージ。** agy がツールを拒否したとき、gateway が返すエラーは拒否されたツールを名指しし、agy 自身のエラーテキストを添えます。キーはテキストを切り詰める前にマスクされます。agy が成功を報告しても返信が空で、ツールが拒否されていた場合、その実行はエラーになります。以前は、結果の生の JSON が従業員の回答になっていました。通常の返信があってツールが拒否されただけの場合は、返信を保持し、警告を 1 件記録します。

**未検証の項目。** このコードを最後に変更した後、実際のキーによるエンドツーエンドのテストはまだやり直していません。Linux、Docker コンテナ内、Windows でも試していません。Antigravity には既知で未対応の問題が 2 つあります。agy が 503 の再試行に成功した後でも失敗を報告することがあり、gateway は完全な返信を失敗として扱います。また、Antigravity の失敗後に、ベンダーをまたぐフェイルオーバーが Claude に切り替わることがあります。

### Antigravity のストリーム解析は失敗ではなく縮退する（2026-09-28）

`agy --output-format stream-json` のデコーダーは、以前は 6 か所で独立して厳格でした。解析できない行が 1 行ある、`result` イベントがない、`response` フィールドがない、`usage` ブロックの整数が 1 つ足りない、のどれかで実行全体がエラーになり、*すでに回答済み*の `agy` が spawn 失敗として報告され、ロールメンバーも失われました。

形の不一致は縮退して扱い、事実は縮退させません。解析できない行はスキップします。result がなければ、ストリームの最後の空でない行を回答として使います。`usage` ブロックがない、または不完全な場合は、でっち上げた 0 ではなく不明なトークン数になります。縮退のたびに、何が欠けていたかを示す `warn!` を 1 件出します。いまも即座に失敗する唯一のケースは、明示的な `SUCCESS` 以外のステータスです。これは `agy` が実行の失敗を伝えているのであって、こちらが認識できなかった形ではありません。

### フォールバックのランタイムが受け取るモデル（2026-09）

別のランタイムへフェイルオーバーするとき、元のモデル id をそのまま転送することはありません（Codex エージェントの `gpt-5.4` を Claude CLI に渡してはいけません）。FailoverManager は、順序のある 4 つの分岐でフォールバック先のモデルを解決し、どれも当てはまらなければ spawn を拒否します。

1. `agent.toml [model] fallbacks` のうち、そのファミリーが確実にフォールバック先のランタイムに属する最初のエントリー（`openai/gpt-5.4` のような修飾付き id は、Direct-API チェーンが使うのと同じ `split_model_id` の規則で修飾を外します）。
2. 元のモデル。すでにフォールバック先のランタイムに属している場合。
3. そのランタイムのカタログ既定値（`fallback_models[0]`。ライブ探索が失敗したときにダッシュボードが提示するのと同じリスト）。
4. いずれでもなければ、この試行は `no model configured for fallback runtime <name>` として失敗扱いになり、spawn しません。

置き換えが起きるたびに、`agent / from_runtime / to_runtime / from_model / to_model` を `warn` レベルで記録します。

**judge と evaluator の呼び出しは、ファミリーをまたぐフェイルオーバーから完全に外れます**（2026-09-28 訂正）。オペレーターが judge のランタイムまたはモデル（`[dispatch] judge_provider` / `judge_model`）を指定した場合、その呼び出しの目的は*どのファミリーが答えるか*にあります。そのため失敗した judge の spawn を別ファミリーのモデルで救済することはなく、呼び出し側が明示的に、見える形で縮退します。この除外は以前、judge のヒントがプロバイダーを解決済みのデフォルトから*動かした*ときにしか働きませんでした。そのため judge のファミリーとデフォルトの utility のファミリーがたまたま同じ場合（たとえば両方が `codex`。Codex もデフォルトの utility ランタイムにしたとき、去相関 judge の構成が行き着く状況です）には、置き換えが黙って復活していました。現在は、ファミリーを名指しすればそのファミリーを要求したことになり、それがデフォルトでもあるかどうかは関係ありません。ヒントなしの utility 呼び出しのフェイルオーバーは従来どおりです。

- **Account Rotator**：全プロバイダーの認証情報を管理、クロスプロバイダーフェイルオーバー付き。
- **Confidence Router**：runtimeレイヤーの下位に位置——ローカル vs. クラウドを決定。Runtimeレイヤーは*どの*クラウドかを決定。
- **CostTelemetry**：プロバイダーごとのコストを追跡し、情報に基づくルーティング決定を支援。
- **MCP Server**：ツールはサポートする全バックエンドに公開（ClaudeはネイティブMCP経由、その他はツールインジェクション経由）。
- **Agent Config**：各エージェントの`agent.toml`がruntimeの優先設定とフォールバックチェーンを指定します。

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

AI領域はマルチプロバイダーです。単一CLIの上に構築するのは、単一OSのためだけにソフトウェアを書くようなもの——動くけど、いつか動かなくなります。`AgentRuntime` traitが差異を抽象化し、DuDuClawがClaude、Codex、Gemini、そしてOpenAI互換エンドポイントを交換可能なバックエンドとして扱えるようにします。エージェントは常に、利用可能な最良の頭脳を得られます。
