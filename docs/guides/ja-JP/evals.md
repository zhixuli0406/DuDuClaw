# エージェント行動 eval（`duduclaw eval`）

エージェント向けの golden-task **行動回帰テスト**。各 case は、gateway が使っているのと**同じ CLI harness の呼び出し方**（stream-json 出力、`[capabilities]` のツール許可／拒否リストの配線、per-agent の `.mcp.json`、`--max-turns` 予算）を通じて 1 つの prompt をエージェントに送り、生成された transcript を解析し、確定的なアサーションと任意の LLM judge によるルーブリックでチェックします。

これは ADK-evalset／Braintrust の eval-action パターンを DuDuClaw に取り込んだものです。1 つの case が 1 つの TOML ファイルに対応し、CI をゲートする exit code があり、さらにオフラインの replay モードによって、token を使わずに回帰を検知できます。

> **これが自己進化するプラットフォームにとって重要な理由。** DuDuClaw の GVU ループは
> `SOUL.md` を書き換え、その変更を自分自身の Verifier で検証します。この Verifier は
> ループの*内側*にいます。自分が採点している対象と一緒にドリフトする可能性があるということです。
> eval は**外部の物差し**です。人が書いて固定された、期待される振る舞いの集合であり、
> プロンプトの変更、runtime／provider の入れ替え、`claude` CLI のアップグレード、GVU による
> `SOUL.md` の書き換えのどれが起きても、**気づかれないまま後退することはありません**。
> 詳細は下記の [外部の物差し](#進化との統合外部の物差し) を参照してください。

---

## クイックスタート

```bash
# オフラインモード（エージェント不要、認証情報も不要。決定論的な回帰）：
duduclaw eval evals/examples/greeting-replay.toml --replay
duduclaw eval evals/examples/grounded-replay.toml --replay

# live モード（実際のエージェントを動かし、後で replay できる基準 transcript を記録）：
duduclaw eval evals/examples/refund-flow.toml --record

# suite 全体を実行（再帰検索、ソート順）、機械可読なレポートを出力：
duduclaw eval evals/support --report eval-report.json
```

`PATH` には単一の `*.toml` case ファイル、**または** suite ディレクトリ（再帰的に検索し、ソート順に実行）を指定できます。デフォルトは `./evals` です。

### フラグ

| フラグ | 説明 |
|------|------|
| `--filter <substr>` | `[case] name` に `<substr>` を含む case だけを実行します。部分文字列マッチなので一意性は保証されません。一意に選ぶには下記の `--case` を使ってください。 |
| `--case <id>` | 安定した id（＝ case ファイルの**ファイル名の幹**、例：`p0-ceo-boundary-money-001`）で case を厳密に選択します。繰り返し指定、またはカンマ区切りで複数指定できます。実行するかどうかを判断するためだけに case を読み込むことはなく、`--filter` のような曖昧さも起きません。 |
| `--exclude-dir <name>` | 指定した名前のディレクトリ配下の case ファイルを除外します（繰り返し指定可）。例えば `--exclude-dir held-out` で held-out ローテーションをスキップできます。省略すればこれまでどおりすべてを含みます（デフォルト挙動は変わりません）。 |
| `--replay` | 記録済みの `*.transcript.jsonl` ファイルを解析します。live でエージェントを動かしません（オフライン、認証情報不要）。`--record` とは併用できません。 |
| `--record` | 一度 live で実行し、各 case の隣に `*.transcript.jsonl` の基準ファイルを書き出します。case は `[case] runtime` と `[case] model` を固定することで、Claude 以外の基準を記録できます。その transcript は、その runtime が観測できるイベントから合成されます。`--record` では CLI の `--model` による上書きと、Claude 以外の runtime を指定する CLI の `--runtime` 上書きは引き続き禁止されており、別の宣言をされた基準を 1 回の実行が黙って上書きしないようにしています。 |
| `--no-judge` | case が `[judge]` を有効にしていてもスキップします（完全に決定論的、コストゼロ）。 |
| `--report <path>` | JSON レポートを書き出します（各 case のアサーション結果、judge のスコア／理由、transcript の診断、所要時間、そして [誠実な統計](#誠実な統計) で説明する `stats` ブロック）。 |
| `--repeats <N>` | 各 case を `N` 回実行し、ノイズの多い 1 回きりの 0/1 の代わりに合格**率**を集計します（デフォルト `1`、挙動は変わりません）。[誠実な統計](#誠実な統計) を参照してください。 |
| `--baseline <report.json>` | 以前に書き出した `--report` ファイルとの対応のある統計的比較を行います。[誠実な統計](#誠実な統計) を参照してください。 |
| `--mde <fraction>` | 解像度チェックのために宣言する最小検出効果。合格率の割合で指定します（デフォルト `0.10` ＝ 10 パーセンテージポイント）。[誠実な統計](#誠実な統計) を参照してください。 |
| `--cluster-by <key>` | クラスターロバスト標準誤差のためのクラスターキー。実装されているのは `dir`（デフォルト、各 case のディレクトリ）だけで、それ以外の値は拒否されます。[誠実な統計](#誠実な統計) を参照してください。 |
| `--runtime <id>` | すべての case を実行するバックエンド（`claude` がデフォルトで P2 以前の経路、ほかに `codex`、`gemini`（v1.67.0 で非推奨、v1.69.0 で削除。[deprecations](deprecations.md#gemini-cli-ランタイム) を参照）、`antigravity`、`grok`、`openai_compat`、またはカタログ内のその他の runtime id）。未知の id は拒否され、`claude` として扱われることはありません。**省略した場合は各 case 自身の `[case] runtime`、それもなければ `claude` です。** [能力マトリクス](#能力マトリクス--matrix) を参照してください。 |
| `--model <id>` | `--runtime` の範囲内で、すべての case に対する model id の上書き。省略すると各 case 自身の `[case] model` を使います。レポートヘッダーの `model` には、常に実際に動いたものが書かれます。 |
| `--paired-seeds` | `(case id, repeat)` ごとに決定的な seed を導出し、同じ抽選が model 間で揃うようにします（Miller の対応のある設計）。**記録するだけで適用はしません**。このビルドの runtime には seed を受け付けられるものがなく、各実行は `seed_applied: false` でそのことを示します。 |
| `--agent <id>` | すべての case を、各 case 自身の `[case] agent` の代わりに**この**デプロイ済みエージェントで実行します。レポートヘッダーに `agent_override` として、各実行に `agent` として記録されます。[1 つのエージェントを借りる](#1-つのエージェントを借りる--agent) を参照してください。 |
| `--matrix` | suite を 1 回実行する代わりに、role→model の能力マトリクスを測定します。[能力マトリクス](#能力マトリクス--matrix) を参照してください。`--roles`、`--models`、`--weak`、`--strong`、`--domain`、`--budget-usd`、`--max-cases`、`--temperature` が付随し、これらは `--matrix` なしでは**拒否されます**。 |

`--record` は、Claude 以外の runtime を指定する CLI の `--runtime` 上書き（`--runtime claude` は受け付けます）や、CLI の `--model` による上書きと併用すると**拒否されます**。そうしないと、各 case のコミット済みの基準 transcript が、その case が宣言していない model の実行結果で置き換わってしまうためです（Claude 以外の runtime では、忠実度の異なる合成 transcript にもなります）。代わりに case ファイルの中で `[case] runtime` と `[case] model` の両方を固定し、上書きなしで記録してください。CLI による上書きがないとき、実際の実行に使われるのはこの 2 つのフィールドです。CLI による上書きは引き続きそれらより優先されます。

**case の id と suite の一意性。** 各 case の安定した id は、そのファイル名の幹です（`[case] name` は人が読むためのタイトルのままで、id ではありません。`--filter` は `name` を、`--case` は id を対象にマッチします）。同じ実行の中で 2 つの case ファイルが同じファイル名の幹を共有している場合、suite は読み込み時点で即座に失敗します。id が黙って衝突すると `--case` が曖昧になってしまうからです。

**exit code：** 1 つでも case が失敗すれば、プロセス全体が**非ゼロ**の exit code を返すので、そのまま CI ゲートに組み込めます。コンソールには人が読める表形式が出力されます。`--report` ファイルはその機械可読版で、既存の詳細な `cases` 配列に加えて、`{suite, total, passed, per_case: [{id, name, passed, failed_assertions, judge_score, mast_class}]}` という簡潔な構造、`mode` と並ぶ `model` ヘッダー、そして `stats` ブロックとトップレベルの `verdict`／`label`／`resolution_ratio_q`（[誠実な統計](#誠実な統計) を参照）も持つようになりました。gateway の `eval_runner` のようなプログラムからの利用者向けです。

---

## Case フォーマット

1 つの case は 1 つの TOML ファイルに対応します。

```toml
[case]
name   = "refund-flow"          # [a-zA-Z0-9_-]、64 文字以内。レポートに表示される
agent  = "support-bot"          # ~/.duduclaw/agents/<agent> 配下の agent id
prompt = "A customer asks for a refund on order #1234. Handle it."
# system_prompt = "..."         # 任意：--system-prompt-file 経由で渡す
# model         = "claude-haiku-4-5"   # デフォルト：claude-sonnet-4-6
# runtime       = "codex"       # この case のバックエンドを固定する。未指定 = claude。
                                #   [case] model が必要で、その model のファミリーは
                                #   runtime に属している必要がある。CLI の --runtime が
                                #   指定されればそちらが優先される。--record では（CLI
                                #   による上書きが拒否されるため）、Claude 以外の基準を
                                #   記録する唯一の方法がこれ。
# team_acceptance = "..."       # この case を完全な team ラウンド経由で動かす場合
                                #   （duduclaw eval --team-2x2）に使う受け入れ基準。
                                #   通常の実行では無視される。
# timeout_secs  = 180           # live 実行時の wall clock 上限（1..=3600）
# max_turns     = 25            # CLI の --max-turns（1..=100）
# transcript    = "custom.jsonl" # replay ファイル。この case ファイルからの相対パス。
                                #   デフォルト：<case ファイル名の幹>.transcript.jsonl

[expect]                        # すべてのフィールドは任意。「設定された」フィールドごとに
                                # レポート上にちょうど 1 件のアサーションが生成される
must_use_tools     = ["tasks_create"]  # 最低 1 回は呼び出されなければならない
must_not_use_tools = ["Bash"]          # 一度も呼び出されてはならない
output_contains     = ["1234"]         # 最終回答に含まれる部分文字列（大文字小文字を区別）
output_not_contains = ["sk-ant-"]      # 最終回答に含まれてはならない
output_regex        = "(?i)refund"     # 最終回答が一致すべき Rust の regex
min_text_blocks     = 1                # assistant のテキストブロックが N 個以上
max_tool_calls      = 10               # tool_use ブロックは N 個以下（budget guard）

# trace-grounding のアサーションは 0 個以上。詳細は下記の「Trace grounding」を参照
[[expect.grounded]]
tool               = "memory_search"   # 最低 1 回はエラーなしで呼び出されなければならない
min_overlap_chars  = 12                # デフォルト 12。CJK-safe な文字数
# output_regex     = "30 days"         # 任意、詳細は下記を参照

[judge]                         # 任意の LLM ルーブリック（Braintrust の scorer 方式）
enabled   = true                # [judge] セクションが存在すればデフォルトで true
rubric    = "Politely acknowledges the refund and cites the order number."
min_score = 0.7                 # score >= min_score で合格（0.0..=1.0）
```

読み込み時には以下のルールが強制されます（フェイルファスト。タイプミスで suite が中途半端に実行されることはありません）：

- case は `[expect]` のアサーションを**最低 1 つ**定義するか、**または** `[judge]` を有効にしていなければなりません。チェック項目が 1 つもない case は拒否されます。
- **未知のフィールドは拒否されます**。例えばタイプミスの `tool_calls_includ` は静かに通過するのではなく、読み込み時点でそのまま失敗します。
- `output_regex` はコンパイルできなければならず、`min_score` は `0.0..=1.0` の範囲、`timeout_secs` と `max_turns` にも範囲チェックがあります。`transcript` のパスは絶対パスにできず、`..` も含められません（case ファイルを使って任意のファイルを読ませる罠を仕掛けられないようにするためです）。
- 形式が壊れた case は、**理由付きの FAILED case** として報告され、スキップされることは絶対にありません。壊れた suite が CI をこっそりグリーンにすることはできません。

### ツール名のマッチング

`must_use_tools` / `must_not_use_tools` はツール名を**完全一致**か、末尾の `__` 区切りセグメントのどちらかでマッチさせます。これは token アンカー型のマッチであり、生の部分文字列マッチではありません。したがって `tasks_create` は `mcp__duduclaw__tasks_create` にマッチしますが、`create` は `tasks_create` に**マッチしません**（これはプロジェクトの「セキュリティ／ルーティング判断にアンカーなしの `contains` を使わない」という慣例に従っています）。

### 「output」とは何を指すか

アサーションが対象にするのは、stream-json の transcript から解析された**最終回答テキスト**です（空でない `result` イベントがあればそれを使い、なければ最後の assistant のテキストブロックを使います）。これは gateway 自身の stream parser が使っているのと同じ優先順位です。ツール関連のアサーションは、順序付きの `tool_use` ブロックのリストを対象にします。regex と部分文字列のチェックはどちらも UTF-8／CJK-safe です（Rust の `regex` を使い、バイト単位のスライスは行いません）。

---

## Trace grounding（`[[expect.grounded]]`、GroundEval）

worker は、流暢で話題に合った最終回答を出しつつ、その中身を**でっちあげる**ことがあります。`memory_search` を一度も呼ばずに「返金ポリシーを確認しました、30 日以内なら返金可能です」と言い切ったり、呼びはしたものの、ツールが一度も返していない数字を引用したりするケースです。`must_use_tools` はツールが*呼ばれたかどうか*しかチェックせず、最終回答がそのツールの返した内容を実際に反映しているかどうかは一切見ていません。`[[expect.grounded]]` はこの隙間を埋めるために存在します（GroundEval、arXiv:2606.22737）。

```toml
[[expect.grounded]]
tool              = "memory_search"  # must_use_tools と同じマッチ方式（完全一致か
                                      # 末尾の `__` 区切りセグメント）
min_overlap_chars = 12               # デフォルト 12
output_regex      = "30 days"        # 任意
```

grounded のアサーションは、以下の**すべて**を満たしたときだけ合格します。

1. `tool` が最低 1 回呼ばれており、その呼び出しの `tool_result` に `is_error` が**ない**こと。
2. 最終回答が、そのツールの結果テキストの少なくとも 1 つと、**`min_overlap_chars` 文字以上連続する**内容を共有していること（CJK-safe：バイトではなく `char` 単位でカウントするので、12 文字の日本語の一節は 12 であり 36 ではありません）。
3. `output_regex` が設定されている場合、最終回答内でそれがマッチした部分文字列は、そのツールの結果テキストのどれかに一字一句そのまま現れていなければなりません。*回答自体*だけで regex がマッチしても十分ではなく、引用された事実が証拠の中に一度も現れていなければ、それだけで失敗になります。

このチェックには、`tool_result` が取り込まれた transcript が必要です（この機能と同時に追加されました）。`tool_result` の取り込み機能が存在する前に記録された transcript、あるいは `tool_calls.jsonl` に相当する結果ストリームが失われた case 経由で読み込まれた transcript では、このアサーションは**閉じた形で失敗**し、詳細情報として新しい transcript を `--record` するよう案内が表示されます。証拠が欠けている状態を黙って合格にすることはありません。

### この証拠が使われるもう一つの場所：goal-mode の受け入れ判定

同じ tool-call の証拠は、**goal-mode の受け入れ judge**（`DispatchEngine::review_goal_tasks`、WP4）にも供給されます。`review` タスクを採点する前に、judge はそのタスクの claim から review までの間の `tool_calls.jsonl` を読み込み、簡潔な `<tool_activity>` ブロック（ツールごとに `tool: N ok, M err`、最大 20 行）を受け入れ prompt に添付します。`correctness` の観点では、worker が*主張した*にもかかわらず `<tool_activity>` に一切現れないアクションは、未検証として扱うよう明示的に指示されています。これは best-effort な仕組みです。監査ファイルが欠けている、あるいは読み込めない場合は、このブロックが省略されるだけで、可観測性の欠落を理由に受け入れ判定がブロックされることはありません。

---

## live と replay

| モード | コマンド | 必要なもの | 用途 |
|------|------|------|------|
| **live** | `duduclaw eval evals/support` | デプロイ済みのエージェント＋環境にある `claude` の認証情報 | case の作成、リリース前の行動チェック |
| **live + record** | `duduclaw eval evals/support --record` | 同上 | 回帰の基準（`*.transcript.jsonl`）を（再）作成する |
| **replay** | `duduclaw eval evals/support --replay` | 何も要らない（オフライン） | 決定論的なアサーションに対する CI の回帰ゲート |

- live 実行は**エージェントのディレクトリの中**で行われ、そのエージェントの `[capabilities]` の許可／拒否ツールリストが適用され、per-agent の `.mcp.json` があればそれも適用されます（`--strict-mcp-config`）。使われるのはコマンドを実行した人がログインしている `claude` アカウントで、複数アカウントのローテーションはありません。eval はオペレーター／CI 向けのツールであり、チャネルの経路ではありません。
- case は意図的に**単発でセッションを持たない**（`--resume` を使わない）ように設計されており、再現性を確保します。
- `[judge]` のルーブリックは **replay** でも実行されます（記録済みの最終回答を採点します）。`--no-judge` を付ければ、完全に決定論的でコストゼロの実行になります。

典型的なワークフロー：case を書き、まず `--record` を一度実行して既知の良い transcript を記録し、`*.transcript.jsonl` を commit します。その後、すべての PR で CI に `--replay` を実行させます。行動の変化を*意図的に*起こしたいときだけ、`--record` で基準を更新してください。

記録の隔離：spawn 時、runner はそのエージェントの `.mcp.json` を**一時的なコピー**に書き換え、その `DUDUCLAW_HOME` を eval home に向けます（`DUDUCLAW_MCP_API_KEY` はプレースホルダーの値になります）。そのためサンドボックスの home の中で記録しても、ツールの副作用が本番環境に書き込まれることはなく、本番環境から認証情報が漏れることもありません。元のファイルが書き換えられることは決してありません。

暴走した実行は失敗として扱われるだけで、致命的ではありません。live 実行がエージェントの `max_turns` 上限（無限のツールループ）に達して停止した場合、`error_max_turns` として記録されます。transcript は解析可能なままで、アサーションもエージェントが実際に行ったことに対して実行され、その case は行動面の失敗の基準として結果に計上されます。インフラ層のエラー（spawn の失敗、認証情報のエラー、transcript の形式破損）だけがハードエラーとして扱われます。

---

## 誠実な統計

生の合格率（「10 件中 7 件の case が合格」）は統計量とは言えません。誤差範囲がなく、case が少数だと、運を本物のシグナルと取り違えやすいからです。eval レポートが **role → model の能力マトリクス**のデータ源になるときに、これは最も重要になります。検出力の足りない suite で model を比較すると、本物の勝者を見つけるのと同じくらい簡単に、偽の勝者が作り出されてしまいます。

`duduclaw eval` は、A/B テストと同じ方法で数値を計算します。根拠は 3 本の論文です。

- **Miller 2024「Adding Error Bars to Evals」**（arXiv:2411.00640、Anthropic）：独立した合格率ではなく*問題ごとの*差に基づく対応のある比較、case が構造を共有する場合（ここでは置かれているディレクトリ）のクラスターロバスト標準誤差、そして実際に何問・何回の繰り返しが必要かを計画するためのサンプルサイズの計算。
- **「Resolution Diagnostics」**（arXiv:2605.30315）：比較が意味を持つのは、suite が*宣言された*最小検出効果（MDE）を解像できるだけの大きさを持つときだけです。`q = n / n_required < 1` は `unresolved` として報告しなければならず、黙って勝者に切り上げてはいけません。
- **The Replay Gap**（arXiv:2608.08239）：`--replay` の実行は、過去のある model 実行の*固定された* transcript を解析します。これを*別の* model の live 実行と比べると、実際には起きていない能力差が作り出されます。

### 計算される値

すべての実行で `stats` ブロック（`--report` の JSON に埋め込まれる）が計算され、1 行のコンソールサマリーが出力されます。

```
n=42 clusters=6 pass=83.3% ±7.1pp (clustered) | MDE@n=10.0pp | q=1.84 → pass (vs chance)
```

- **`n` / `clusters`**：異なる case の数（`--repeats N` は各 case の `N` 回の実行を、先に 1 つの合格*率*へ集約します）と、異なるクラスター（ディレクトリ、`--cluster-by dir`）の数。
- **`pass ±X pp (clustered)`**：suite の平均合格率と、**クラスターロバスト**標準誤差から計算した 95% 信頼区間の半幅（Miller 2024 §2.2／付録 C）。同じディレクトリ内の case は互いに相関してよいものとして扱われ（例えば fixture、prompt テンプレート、不安定なツールを共有している場合）、すべての case が独立したコイン投げであるかのように装う代わりに、クラスター標準誤差がそれを考慮します。
- **`MDE@n`**：現在の `n` が 95%／検出力 80% で実際に解像できる最小の効果（式 10）。これが本当に気にしている効果よりずっと大きいなら、比較を信頼する前に case か `--repeats` を増やす必要があります。
- **`q`**：`--mde` に対する解像度比 `n / n_required`（式 9）。`q < 1` なら、点推定がどれほど良く見えても、suite は原則として `unresolved` と報告します。
- **`→ pass|fail|unresolved (vs chance|vs baseline)`**：結論の語と、それがどの問いに答えているか。下の [トップレベル verdict の優先順位](#トップレベル-verdict-の優先順位) を参照してください。この接尾辞はまさにそのために存在します。これがないと、`--baseline` を付けた実行のトップレベル verdict と、自身の `stats.suite.verdict` が正反対の語を出力し、バグのように見えてしまうためです。
- `n_clusters < 5` のときは `WARNING: only <k> clusters` の行が出力されます。それ以下では `se_clustered`（Miller 2024 付録 C）がクラスター間の分散成分をそもそも確実には推定できません。`stats.suite.small_cluster_warning` が JSON 内で同じシグナルを運びます。`se_ratio`（クラスター標準誤差 ÷ クラスターなしの単純な標準誤差）が `2` を超えたときは、2 つ目の `WARNING` 行が出力されます。ディレクトリ内の case が強く相関していて、クラスターなしの数値は自信過剰になる兆候です。（実機での例：2 クラスターの suite が `se_ratio: 0.27` を報告しました。クラスターがこれほど少ないと、この比そのものに意味がありません。クラスター数の警告が `se_ratio` の警告と別に、独立して存在するのはこのためです。）

### `--repeats N`：K 回の繰り返しサンプリング

```bash
duduclaw eval evals/support --repeats 5 --report report.json
```

すべての case を `N` 回実行し、確率的な 1 回きりの 0/1 の代わりに、その**合格率**（例：`3/5`）を集計します。繰り返しの transcript はファイル名に番号が入る（`<case>.transcript.r1.jsonl` … `.r5.jsonl`）ので、互いを上書きすることも、`N=1` で使われる通常の `<case>.transcript.jsonl` 基準を上書きすることもありません（デフォルトの `--repeats 1` は既存の挙動とバイト単位で同一です）。*同じ* prompt に対する LLM の繰り返しサンプリングは相関します（文脈や採点の甘さを共有するため）。そのため分散は、独立サンプリングのように `N → ∞` で 0 に近づくことはなく、1 回の抽選の分散の 1/3 で頭打ちになります（`Var(mean|K) = Var(mean|K=1)·(1+2/K)/3`）。繰り返しを増やすことは依然として役に立ちますが、素朴に想定するほどは効きません。

**`--repeats N > 1` は live 実行が必要で、`--replay` との併用は拒否されます。** 繰り返しが測るのは、同じ case の `N` 個の独立したサンプル間の*実行ごとの*ばらつきです。固定された `--replay` の transcript は、測るべきばらつきを持たない 1 つの固定サンプルです（Replay Gap、arXiv:2608.08239 の縮小版で、再生できるものは常に 1 つだけです）。実際に `N` 個のサンプルを取るには、live で `--repeats N --record` を実行し（上記の `.r1.jsonl` … `.rN.jsonl` として書き出されます）、その後 `--repeats 1`（デフォルト）でそれらに対して `--replay` してください。

### `--baseline <report.json>`：対応のある比較

```bash
duduclaw eval evals/support --report candidate.json \
    --baseline previous-report.json --mde 0.05
```

以前に書き出した `--report` ファイルと case を id（`EvalCaseRef`、ファイル名の幹）で突き合わせ、case ごとの**対応のある**差を計算します。2 つの独立した合格率を比べるより統計的に強力です。「この問題はどの model にとっても難しい」という共通の要因を打ち消せるからです。結果（JSON の `stats.baseline_comparison`。採用された場合はトップレベルの `verdict`／`label`／`resolution_ratio_q` も決めます）には以下が含まれます。

- `paired_delta`：対応づいた case についての `candidate_i − baseline_i` の平均。
- `corr_with_baseline`：2 回の実行の case ごとの値の Pearson 相関。これが**負**のとき、対応づけは分散を打ち消すどころか*増やして*しまうため、比較は自動的に対応のない 2 標本の標準誤差にフォールバックし、`fallback_to_unpaired: true` を設定します。
- `ci95_low` / `ci95_high`、`resolution_ratio_q`、`verdict`、`label`。

**Replay Gap に違反する場合は、捏造せず拒否します。** この実行か baseline のどちらかが `--replay` モードで、*かつ* 2 回の実行が異なる model を固定していた場合（レポートには既存の `mode` と並んで `model` ヘッダーがあります）、比較は拒否されます。`baseline_comparison.error` が理由を説明し、トップレベルの verdict は、この実行単独の「合格率 対 偶然」のチェックにフォールバックします。同じ model の replay 対 replay の比較（model 比較ではなく、コードのバージョン間の回帰チェック）は影響を受けません。2 つのレポートの間に重なる case id がない場合も、同様に明示的な `error` となり、引き分けを捏造することはありません。

### トップレベル verdict の優先順位

JSON ルートの `verdict`／`label`／`resolution_ratio_q` と、コンソールサマリーの `→ 語` は、`stats.suite.verdict`／`.label` と**常に同じ計算とは限らず**、両者が正当に食い違うことがあります。実機での例：ある candidate の実行がトップレベルで `→ pass` と出力した一方、`stats.suite.verdict` は `fail`（平均合格率 `15%`）でした。どちらの数値も正しく、答えている問いが違うだけです。

| | 比較対象 | 合格ライン | 答える問い |
|---|---|---|---|
| **トップレベル、`--baseline` が採用された場合** | candidate 対 baseline、対応あり | `0`（差なし） | 「この実行は baseline に対して変化したか？」 |
| **トップレベル、`--baseline` なし（または拒否／重なりなし）** | この実行の合格率 | `0.5`（偶然） | 「この実行の合格率は、コイン投げと区別できるか？」 |
| **`stats.suite.verdict` / `.label`** | この実行の合格率、**常に** | `0.5`（偶然） | 同じ単独の問い。常に計算され、常に報告される。**baseline がトップレベルのフィールドを上書きしている場合でも同じです。** |

つまり `15%` の合格率でも、さらに悪い baseline から改善していれば、トップレベルでは `→ pass (vs baseline)` と出力されることがあります。`stats.suite.verdict: fail` は、それとは別に同時に、`15%` という値そのものは単独の偶然ラインのチェックでは健全な実行と区別できない、と伝えています。どちらを見ているのかは、コンソール行の `(vs baseline)`／`(vs chance)` を読むか、JSON で `stats.baseline_comparison` が null でないかを確認すれば分かります。

### `verdict` / `label`

解像されたすべての点（suite 全体、ディレクトリごとの各行、存在する場合は baseline 比較）は、次のいずれかに分類されます。

| `verdict` | `label` | 意味 |
|-----------|---------|------|
| `unresolved` | `Candidate` | `q < 1`：宣言された `--mde` をそもそも解像するのに case／繰り返し／クラスターが足りない。サンプルサイズの不足であり、判断ではありません。 |
| `unresolved` | `IndistinguishableFromLuck` | `q >= 1` だが、95% 信頼区間が依然として合格ライン（単独の場合は偶然の `0.5`、`--baseline` の場合は差なしの `0`）をまたいでいる。十分なデータが集まったうえで、結果が偶然や baseline と区別できないと示している。 |
| `pass` | `Supported` | 解像済みで、信頼区間が合格ラインより完全に上にある。 |
| `fail` | `Supported` | 解像済みで、信頼区間が合格ラインより完全に下にある。ここでの `Supported` は、*結論*（本物の退行）に証拠の裏付けがあるという意味で、実行が合格したという意味ではありません。 |

`label` は、`duduclaw-gateway::prediction::calibration::HonestLabel` の 3 状態の命名規律を意図的に踏襲しています（task forward model のキャリブレーションチェックで使われるのと同じ `Supported`／`Candidate`／`IndistinguishableFromLuck` の語彙です）。統計量そのものは同じではなく（`calibration.rs` は Sharpe 比の PSR チェックでゲートし、こちらは解像度比と信頼区間が線をまたぐかどうかの検定でゲートします）、プラットフォーム全体の規律は共通です。「うまく動いているようだ」のような、4 つ目のもっと緩い状態を報告することは決してありません。

---

## SOUL.md から suite を組み立てる（`eval-scaffold`）

白紙の状態から最初の case を書くのが一番大変な部分です。しかも playbook の `Add` パイプラインは最低 1 件の eval case のリンク（G6）と E1 アサーションを要求するため、suite を持たないエージェントは新しい playbook エントリーを育てられません。`eval-scaffold` は、あなたがすでに書いたもの、つまりエージェント自身の SOUL.md の行動ルール（identity セクションには一切触れません）から草稿 case を導き出します。LLM は一切使いません。

```bash
duduclaw eval-scaffold --agent my-bot
# → <home>/evals-drafts/my-bot/draft-*.toml、行動ルール 1 件につき 1 ファイル
```

草稿は意図的に**そのままでは実行できない**ようになっています。各 `prompt` は TODO であり、あなた自身が書く必要があります（ツールがユーザーメッセージを勝手に作ることはありません）。また草稿は本番の suite のルートの**外側**に置かれるため、未レビューの草稿が基準を汚染することは決してありません。レビューの流れ：

1. そのルールを実際に引き起こすメッセージを `prompt` に書く。
2. `[expect]` を絞り込む（最低 1 つのツールまたは出力アサーション）。
3. ファイルを `<home>/evals/my-bot/` に移動し、
   `duduclaw eval <そのディレクトリ> --record` を実行する。

このコマンドを再実行しても、あなたが編集済みの草稿が上書きされることは決してありません（再生成したい場合は `--force` を付けます）。

---

## CI の例（GitHub Actions）

replay モードは認証情報を必要としないため、標準的な PR ゲートに向いています。非ゼロの exit code が自動的にこの job を失敗させます。

```yaml
name: agent-evals
on: [pull_request]

jobs:
  evals:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: Build duduclaw
        run: cargo build -p duduclaw-cli --release
      - name: Run behavioral evals (offline replay)
        run: |
          ./target/release/duduclaw eval evals \
            --replay --no-judge \
            --report eval-report.json
      - name: Upload eval report
        if: always()
        uses: actions/upload-artifact@v4
        with:
          name: eval-report
          path: eval-report.json
```

`[judge]` のルーブリックも CI で実行したい場合は `--no-judge` を外してください（`CLAUDE_CODE_OAUTH_TOKEN` か API key も用意します）。夜間の **live** な行動チェックを行いたい場合は、デプロイ済みのエージェントと `claude` のログインを持つセルフホストランナー上で、`--replay` を付けずに同じコマンドを実行してください。

---

## 能力マトリクス（`--matrix`）

Team-as-Agent P2。1 人の AI 従業員は、規劃／執行／審核の各ロールを異なるベンダーの model で動かせます。ここから、suite を 1 回通して実行するだけでは答えられない問いが生まれます。*どのロールの model 選択が本当に効いているのか、そしてこの model はそもそもそのロールをこなせるのか？* `--matrix` はそれを測定します。

### cell とは何か

cell は 1 組の `(domain, role, runtime, model)` です。`domain` は eval suite のディレクトリで、cell のスコアはその suite の case に対して、各 `K` 回測定されます。

| ロール | cell の測定方法 | スコア |
|------|--------------------------|-------|
| **executor** | case の prompt をその `(runtime, model)` 上で live 実行し、確定的な `[expect]` アサーションをチェックします。LLM judge はなく、アサーションが採点のすべてです。 | 合格率 |
| **verifier** | case の**記録済み** transcript と受け入れ基準を model に見せて `PASS`／`FAIL` を答えさせ、同じ transcript に対するアサーションの結果（gold ラベル）と比較します。 | 一致率、および Wilson 区間つきの false-accept／false-reject 率 |
| **planner** | `--team-2x2` は、各 case の 4 つの隔離されたコピー（planner 弱／強 × executor 弱／強）に対して、本番の composer を実行します。verifier は固定です。完全に揃った行だけが Shapley 推定に入ります。通常の `--roles planner` の単発呼び出し経路は引き続き拒否されます。 | 強い executor を使った場合の、独立した verifier の PASS 率 |

joint team の 2×2 Shapley 計算と、その本番 composer プロデューサーは `--team-2x2` から利用できます。形式の壊れたスコアは拒否し、幅ゼロの信頼区間は省略します。修正後の 12 アームの live プローブは、別々のディレクトリにある 3 件の合成 case で planner→executor→独立した verifier の経路を完走しました。4 つの model cell はすべて `n=3` で、`unresolved` のままでした。ルーティングについて何かを主張する前に、より広い、代表性のある複数 domain の live 受け入れ実行が依然として必要です。利用できない verifier の判定は採点対象外で、PASS ではありません。

公開ベンチマークのタスクファイルは、その指示を `[case] prompt` にコピーするだけでは `--team-2x2` に追加できません。インタラクティブなベンチマークには、そのタスク固有のツール、シミュレーターの状態、独立した結果の oracle が必要です。それらの case がこのマトリクスに数えられる前に、team harness がそれらを保持していなければなりません。

verifier cell で transcript を replay するのが正当なのは、まさにテスト対象が worker ではないからです。それを判定するよう求められる model は、常に live で呼び出されます。これが、verifier cell に `--replay` フラグが要らない理由であり（そして `--matrix --replay` が拒否される理由でもあります。下記のハードルールを参照）。

verifier の指標は 1 つではなく 3 つです。合格が多数を占める suite では、何も不合格にしない verifier でも、一致率だけ見れば良く見えてしまいます。**False accept**（gold が `FAIL` なのに `PASS` と言う）は高くつく誤りで、不良な成果物を通してしまいます。**False reject** のコストは修正ラウンド 1 回分で済みます。どちらの判定も含まない返信は **unparseable** として数えられ、3 つの率すべてから除外されます。「解析可能な判定を出せない」ことと「判定が下手」であることは、別の発見だからです。

**判定はエージェントのメッセージから読み取り、stream の行からは決して読み取りません。** メッセージではなく生のイベント stream を返す runtime では、先にメッセージが復元されます（codex の `item.completed`／`agent_message`｜`message` 項目と、claude の `assistant`／`result` イベントに対して、後勝ち）。stream でないもの、あるいは復元できるメッセージのない stream はバイト単位でそのまま通過するため、構造化された判定が書き換えられることはありません。復元が働いた実行には `message_recovered_from_stream` が付けられ、この回避策が見える状態に保たれます。実機での動機（smoke 3）：すべての codex verifier の返信が `unparseable` と採点され、`first_line` は `{"type":"turn.completed","usage":{…}}`、つまり stream の最後のイベントでした。transcript の 1 行を判定として採点しても、何も測れません。

**受け付ける判定の形式は 2 つ。** すべての verifier 呼び出しは、team judge が使うのと同じ `--output-schema` の仕組みを通じて構造化された返信（`{"verdict": "PASS"|"FAIL", "reasons": [...]}`）を要求しますが、これを尊重するのは **codex のみ**で、ほかの runtime はログに残して無視します。パーサーは、その JSON オブジェクト（fenced code block の中でも可、どの runtime でも）**または**単純な先頭 token の `PASS`／`FAIL` 形式を受け付け、それ以外は fail-closed で `unparseable` にします。JSON 形式を先に試すのは意図的です。`{"verdict":"PASS","reasons":["... would FAIL if ..."]}` のような返信は、そうしないと、散文形式の保守的な「最初の行のどこかに FAIL があれば FAIL が勝つ」という同点処理で、自分自身の理由テキストのせいで誤判定されてしまいます。実機での動機：最初の smoke 実行で、codex の verifier cell が 4/4 すべて `unparseable` になりました。prompt でどれほど明確に頼んでも、codex は返信を単独の判定 token で始めるとは限らないためです。

### ボトルネックのヒューリスティックと、それが見えないもの

`--weak`／`--strong` を指定すると、各ロールは**同じ case** 上で Δ = score(strong) − score(weak) を得ます（case ごとの対応のある差と、クラスターロバスト標準誤差。2 つのアームが case id をまったく共有しない場合に限り、対応のない平均の差にフォールバックし、そのことが明示されます）。Δ が大きいロールが、model への支出が最も効くところです。

信頼区間がほかのすべてのロールの区間を除外するときにだけ、それはボトルネックと**呼ばれます**。そうでなければ答えは `unresolved` であり、これは本物の答えです。点推定の順位づけでは間違えてしまうケースです。Δ が解釈できないロールは、比較から**完全に除外**もされます。`degenerate_gold`（suite の再記録が必要）または `degenerate_interval`（suite にもっと case かクラスターが必要）の場合です。拒否の理由には、除外された各ロールとその原因が明記されます。

これは AgentCARD の Shapley プローブ（arXiv:2606.20629）を**分離した**形です。ロールは 1 つずつ独立に測定され、joint team の実行は行いません。これが現実的なコストにしている点（|models|^|roles| 通りの team 構成ではなく、2 ロール × 2 model）であり、同時に構造上それが見えない点でもあります。本物の相互作用（弱い executor の後ろでしか効果を発揮しない強い verifier など）は見えません。出力は「まずどのロールに支出するか」として読んでください。team レベルの帰属として読んではいけません。

### 1 つのエージェントを借りる（`--agent`）

マトリクスが測るのは **model** であり、ペルソナではありません。そのため、この home にはないエージェント向けに書かれた suite（premium の suite に対するプローブ実行でよくあるケース）では、`--agent <id>` がすべての case を 1 つのデプロイ済みエージェントで実行します。

```bash
duduclaw eval commercial/evals/hr-recruit --matrix --agent agnes ...
```

プローブとしては許容できる妥協であり、**宣言されるもので、推論されるものではありません**。すべての case が動くときの system prompt が変わるため、レポートヘッダーに `agent_override` が、すべての `runs[]` 行に実際に使われた `agent` が記録されます。述べておく価値のある帰結が 2 つあります。suite をまたぐ比較は、*同じ* `agent_override` の実行同士でしか成り立たないこと。そして、元のエージェントのツールやペルソナに依存するアサーションを持つ case は、model のせいではなくその理由で失敗することがある、ということです。

`--agent` なしでは、`[case] agent` がデプロイされていない case は、従来と同じ「not found」エラーで失敗します。欠けているのが上書き指定の側である場合、どちらが欠けているか分かるよう、メッセージには**両方**の id が明記されます。

### verifier cell には混合した gold が必要

verifier cell のスコアは、記録済み transcript 上の確定的な gold との一致率です。すべての case の gold が同じクラスなら、一致率は何も測りません。無条件に `FAIL` と答える verifier が、すべて FAIL の gold に対して 1.00 を取ってしまうからです。

そのため cell は `gold_pass`／`gold_fail` の件数を報告し、どちらかのクラスが存在しないときは `degenerate_gold: true` を設定し、`verdict: "unresolved"` を `verdict_reason: "degenerate_gold"` とともに強制し、コンソール行に警告を出力します。統計量そのものは引き続きすべて報告され（`verdict_statistical`／`label_statistical` として並べて保持されます）、何も隠されません。差し控えられるのは*結論*です。degenerate な gold を含む cell を持つロールも、ボトルネックの比較から除外され、その理由には問題のある model が明記されます。

> **P2 の負債。** 出荷済みの premium suite の記録済み transcript は、現在のアサーションに対して古くなっています。2026-09-24 の P0 live テストでは replay で 360 件中 98 件が合格と測定され、前のリリースの binary でも変わりませんでした。そのため、事実上すべての case で gold は FAIL となり、すべての verifier cell が `degenerate_gold` で返ってきます。**意味のある verifier マトリクスを得るには、premium suite を再記録する（`--record`）か、アサーションを修正する必要があります。** executor cell は影響を受けません。live で実行され、記録済み transcript を読むことがないためです。

### `--budget-usd` を意味のあるものにする

コストは `duduclaw_llm::ModelRegistry` で価格付けされます。同梱のテーブルは、live 実行の前に `<DUDUCLAW_HOME>/models.toml` とマージされます。現行の model id と検証済みの価格をそこに追加してください（同じスキーマ：`input_mc`／`output_mc`／`cache_read_mc`、単位は MTok あたりの millicent、`$1/MTok = 100_000` mc）。`--budget-usd` を付けた場合、未知の model の価格があると、最初のディスパッチの前に実行全体が拒否されるようになりました。予算なしの場合は、診断用の実行のために、ラベル付きの `$0.05` のスタブを引き続き使います。レポートの `cost_estimate.models_not_in_registry` には、スタブが使われた id が列挙されます。上限は実行前の token 推定を使います。ある model が使用量を報告した後は、後続の実行はその model で観測された最大の 1 回あたりコストの 2 倍を確保します。これは見積もりに基づく支出ガードであり、provider 側が強制する課金上限ではありません。

### 1 クラスターは不確実性ゼロではない

`--cluster-by dir` では、すべての case が**1 つ**のディレクトリにある suite はクラスターが 1 つだけで、Miller のクラスターロバスト推定量はそこでは*恒等的にゼロ*になります。クラスターが 1 つだと、クラスター間の残差和は構成上ゼロになり、CLT の項を正確に打ち消すからです。これは役に立たない推定量の正しい値であり、これを信頼区間として報告すると点になります：`mean 0.25 ci=[0.25,0.25]`。

そのため、クラスターが 2 つ未満の cell（および Δ）は、代わりに**クラスターなしの CLT 標準誤差**を報告し、そのことを明示します。`se_used` の隣に `se_source: "clt_single_cluster"` が付き、2 つの生の推定量（`se_clt`、`se_clustered`）も透明性のためにレポートに残ります。これは正直ですがより弱い推定です。ディレクトリ内の相関は見えず、だからこそ `small_cluster_warning` は引き続き発火します。コンソール行も使われた推定量を明記するので、1 クラスターの区間が「clustered」とラベル付けされることはありません。

**幅ゼロ**の区間は、それが本物である場合にだけ残ります。`n == 1` か、すべての観測値が同一の場合です。それは推定されていないばらつきであり、精度ではないので、その cell は `verdict_reason: "degenerate_interval"` で `unresolved` に強制され、それを土台にした Δ は `degenerate` とマークされ、[ボトルネック](#ボトルネックのヒューリスティックとそれが見えないもの)の判定はそれに基づいて解像することを拒否します。

> 実機での動機（smoke 3、2026-09-25）：1 ディレクトリの suite のすべての cell が点区間で返り、2 つの Δ はどちらも幅ゼロ（`Δ executor = +0.250 [+0.250,+0.250]`）で、ボトルネックは 4 件の case で**解像済み**と宣言されました。まさにこの層が防ぐべき誤った主張です。同じ欠陥は、通常の単一 suite 経路の suite レベルの行（`stats.suite`）でも洗い出されて修正されました。ディレクトリごとの行は、この理由で最初から CLT 標準誤差を使っていました。

### ハードルール

これらはドキュメントに書かれているだけでなく、コードで強制されています。

- **`--matrix` は `--replay` を拒否します。** 固定された transcript で model を比較するのは Replay Gap（arXiv:2608.08239）です。model A の記録は、model B について何も語りません。
- **`--matrix` は記録を行いません。** ここで記録すると、ある domain の基準 transcript が別の model の実行で上書きされてしまいます。
- **本番より低い `--temperature` の宣言は拒否されます**（Miller 2024 §3.3）。temperature を下げると実行ごとのばらつきが抑えられ、デプロイされたシステムが持たない解像度が作り出されます。このビルドの runtime には temperature のノブを公開しているものがないので、受け付けられた値はヘッダーに記録されるだけで、効果はありません。
- **`q < 1` の cell は `unresolved`** であり、ランキングとして読んではいけません。宣言された MDE はコンソールサマリーに出力され、レポートとマトリクスヘッダーの両方に書き込まれます。
- **別の `(runtime, model)` が回答した実行**（gateway の failover が別のものに差し替えた場合）は、その cell から除外され、`substituted` として数えられます。要求された model の手柄になることは決してありません。
- **実行は厳密に直列**で、CLI の spawn は一度に 1 つです。これらの実行はあなた自身のアカウントのクォータを取り合うため、並列のマトリクスは自分自身をレート制限させ、自分のサンプルを相関させてしまいます。

### Claude 以外の runtime：transcript とは何か

`--runtime claude`（デフォルト）は、P2 以前のすべての実行とまったく同じように `claude` CLI を spawn します。それ以外のすべての runtime は gateway の runtime 抽象化（`runtime_dispatch::run_agent_prompt`）を通るので、各ベンダーの argv の規律は、ここで再実装されるのではなく、それぞれの runtime モジュールに残ります。その transcript は、`(最終テキスト, その runtime 自身のネイティブなツールイベント)` から、CLI の stream-json の形で**合成**されるため、`must_use_tools`／`max_tool_calls`／`[[expect.grounded]]` はすべて、既存の 1 つのパーサーを通じて引き続き動作します。合成されたファイルには `duduclaw_eval_synthetic` のシステムイベントで自己ラベルが付きます。

忠実度についての注意：合成された transcript が持つのは、その runtime のイベント stream が実際に運んだものだけです。つまり 1 つのテキストブロック（そのため `min_text_blocks` が観測できるのは常に `1` だけ）、thinking ブロックなし、そしてツール入力は元の JSON ではなく、コレクターが記録したマスク済みで長さ制限のかかったテキストです。これらのシグナルは 2 つの経路の間で**比較できない**ので、1 つの cell がそれらを混ぜることは決してありません。

Antigravity `agy` 1.2.10 は、その runtime に `stream-json` の終端ツールイベントと、実測の token 使用量を提供するようになりました。live のサンドボックスで拒否されたツールイベントと、ツールを使わない PING eval は検証済みです。ネイティブのツール実行が成功するケースは、別途 live での確認がまだ必要です。verifier cell のドル建て見積もりは、その utility 呼び出しが使用量をマトリクスレポーターに返さないため、依然として粗いものです。

### 出力

`--report <path>` は 2 つのファイルを書き出します。

- **`<path>`**：JSON。`header`（宣言された MDE、α、検出力、K、クラスターキー、`planner: "deferred"`、`replay_forbidden`、宣言された temperature、`agent_override`、要求された `verifier_output_schema`、要求された roles／models／domains）、`cells`（cell ごとに：`n`、`n_clusters`、`mean`、`se_clt`、`se_clustered`、`se_ratio`、`ci95_low/high`、`n_required_for_mde`、`resolution_ratio_q`、`mde_at_n`、`q_note`、`se_used`、`se_source`、`verdict`、`label`、`verdict_reason`、`verdict_statistical`／`label_statistical`、`degenerate_gold`、`degenerate_interval`、`small_cluster_warning`、`errors`／`skipped`／`substituted`、さらに verifier cell には `gold_pass`／`gold_fail`／`unparseable`／`degenerate_gold` を含む `verifier` の集計）、`bottleneck`（ロールごとの Δ と区間、および resolved／unresolved の結果とその理由）、`cost_estimate`、`budget_stop`、そしてすべての `runs[]` 行。
- その隣の **`role_model_matrix.toml`**：team composer が読み込む永続的な事前分布です。composer が読むのは `<DUDUCLAW_HOME>/role_model_matrix.toml` なので、有効にするにはファイルをそこへコピーしてください。用途は 2 つです。`[team.roles.*]` で `model` を書いていないロールは、そのロール自身の runtime 上で最も良い **resolved** の cell を採用します（domain をまたいで n で重み付け。unresolved の cell、同点、別のモデルファミリーの勝者、このホストに CLI や認証情報がない runtime はすべて無視され、従業員の `[model] preferred` に戻ります）。また、チームゲートの capability gap シグナルは、executor の現在のモデルとこの勝者を比べ、同じファイルで宣言された MDE と照らし合わせます（[Goal loop のゲート](goal-loop.md#ゲート)を参照）。検証に通らないファイルは無視されます。レポートヘッダーの `planner: "deferred"` は、`--matrix` 自体が planner ロールを測定しないことを示すだけです。1 つの `[header]` と、測定された cell ごとに 1 つの `[[cell]]` で構成されます。計算できなかった統計量は**キーが存在しない**形で表され、数値を捏造することは決してありません。使える観測が 1 件もない cell には、レポートの行はあってもマトリクスの cell は作られません。このファイルは書き込み時にも読み込み時にも検証されるため、手で編集して作った重複した cell や未知の runtime id は、信じられるのではなく拒否されます。各 cell は `conditioned_on` も持ちます。これは、その cell が測定されたとき**ほかの**ロールが何に固定されていたかを示す短い token です。`--matrix` は 1 回に 1 ロールだけを実行し、ループ内にほかのロールの model はないため `solo` と書きます。`--team-2x2` のプローブは、planner のアームを強い executor（`executor=strong`）で、executor のアームを強い planner（`planner=strong`）で測定します。`conditioned_on` が異なる cell は、同じ条件で測定されたものでは**なく**、直接比較してはいけません。このフィールドが存在する前は、ファイル内にそれを示すものは何もありませんでした。キーがない場合は、プロデューサーが条件を記録しなかった（古いファイル）という意味で、空白で書かれることはありません。

### コストと予算上限

`--budget-usd <cap>` は、各実行をディスパッチする**前に**チェックするので、上限は超過したことを報告するだけのものではなく、支出の天井になります。コストは `duduclaw_llm::ModelRegistry`（同梱テーブルと `<DUDUCLAW_HOME>/models.toml`）で価格付けされます。runtime が使用量を返すならそれが実際に報告した使用量から、そうでなければ粗い 1 回あたり入力 25k／出力 4k という想定（設計自身のコストモデル）から算出します。registry がまったく知らない model は、ラベル付きの一律 $0.05 のスタブで価格付けされますが、これは `--budget-usd` がない場合に限られます（予算がある場合、そうした model があると実行全体が拒否されます。[`--budget-usd` を意味のあるものにする](#--budget-usd-を意味のあるものにする)を参照）。この 3 つはすべて `runs[].cost_source` で実行ごとにラベル付けされ、権威ありげな 1 つの数字に混ぜられることは決してありません。Claude CLI の経路は使用量をまったく報告しないため、Claude だけのマトリクスはすべて粗い想定で価格付けされる点に注意してください。

### Smoke 実行

```bash
duduclaw eval commercial/evals/hr-recruit --matrix \
  --roles executor,verifier \
  --models claude:claude-haiku-4-5,codex:gpt-5.6-sol \
  --weak claude:claude-haiku-4-5 \
  --strong claude:claude-sonnet-4-6 \
  --repeats 1 --max-cases 6 --mde 0.10 \
  --agent agnes \
  --report reports/matrix-smoke.json
```

完全な team のプローブでは、選択したすべての case に固定の `[case] team_acceptance = "..."` を追加します。隔離された eval home で、デプロイ済みのエージェントに対して実行してください。

```bash
duduclaw eval commercial/evals/hr-recruit --matrix --team-2x2 \
  --agent agnes --planner-weak codex:gpt-5.6-terra \
  --planner-strong codex:gpt-5.6-sol \
  --executor-weak codex:gpt-5.6-terra \
  --executor-strong codex:gpt-5.6-sol \
  --verifier-model antigravity:gemini-3.7-flash \
  --team-effort low \
  --repeats 1 --max-cases 4 --report reports/team-2x2.json
```

このコマンドには、live 呼び出し、明示的なレポートパス、そしてどちらの executor アームとも異なる model ファミリーの verifier が必要です。composer が seed を適用できないため、`--paired-seeds` は拒否されます。各アームは eval home をコピーし、1 つのタスクを作成し、本物の composer のラウンドを 1 回実行します。業務のタスクとツールが、コピー元の home を変更することはありません。プローブは、アームをコピーする前に `config.toml` に内部用の MCP key を用意するので、gateway プロセスが一つも起動していなくても、それらのロールメンバーが `team_handoff` サイドカーを認証できます。各ロールの MCP 子プロセスは、呼び出し元の環境変数がコピー元の home を指している場合でも、そのアームの `DUDUCLAW_HOME` を受け取ります。`--team-effort` はすべてのアームに同じ reasoning effort を固定してレポートに記録します。省略すると、各 model に設定されたデフォルトが使われます。`--team-fanout` は 1 ラウンドで受け入れる executor の数（1〜3、デフォルト 1）を設定し、レポートに記録されます。実行前の見積もりは、executor の修復 1 回分の可能性も確保します。case の受け入れ基準は、この容量に収まるようにしてください。`team_handoff` パケットなしでテキストだけを返す planner は検証に到達せず、そのアームは採点されません。不完全な 4 アームの行から、executor や planner の cell が出力されることはありません。サンドボックスが起動できないホストでの、隔離された Grok の live プローブでは、`--team-grok-sandbox-off` が、その評価ラウンドに限って明示的に Grok へ `--sandbox off` を渡します。レポートには `grok_sandbox_off: true` が記録され、通常の gateway 経路とロールのツール制限は変わりません。レポートのコストは、使用量が測定できればその実測値、そうでなければ粗い見積もりです。コンソールはそのどちらかを示します。フォールバックした最初の実行で `WARNING` が出力され、プローブの最後には、実行のうちいくつが実測の使用量ではなく粗い見積もりで価格付けされたかが出力されます。最もよくある原因は、コストテレメトリの singleton がプロセス内のどこか別の場所で既にバインドされていることで、その場合 eval home の `cost_telemetry.db` には何も書き込まれません。（そのファイルは読み取り専用で開かれるので、プローブが見ただけで空のファイルを作ってしまうことはありません。）`--budget-usd` は既知の model 価格を事前にチェックし、見積もりの確保額が上限を超えることになる実行の前で停止します。provider の請求書の上限ではありません。各アームの eval home のクローンは、home が 256 MiB より大きい、ディレクトリが 32 階層より深い、またはファイルが 20,000 個を超える場合に拒否されます。クローンはアームごと・繰り返しごとに 1 つ作られるため、スリムな隔離 home を使ってください。出力される TOML に planner cell が含まれるのは、少なくとも 1 件の case が 4 つすべての使える結果を持つ場合だけです。小さなサンプルと degenerate な区間は `unresolved` のままです。別々のディレクトリにある case が同じ TOML ファイル名を共有していても構いません。マトリクスレポートと対応のある統計は、suite 相対パスを case ID として使います（例：`north/checkins`）。`--case` はこの完全な ID も、従来の短いファイル名も受け付けます。後者は、一致するすべてのディレクトリを選択します。

この home に suite 自身のエージェント（`hr-recruit`）がデプロイされているなら、`--agent agnes` は外してください。そうでなければ残します。[1 つのエージェントを借りる](#1-つのエージェントを借りる--agent) を参照してください。

`--max-cases 6` は suite から取る case の数に上限をかけます。`--weak`／`--strong` のアームは、`claude-sonnet-4-6` が `--models` のエントリーでなくても測定されます（そうしないと Δ に比べるアームがなくなります）。6 件の case と `K=1` では、達成される MDE は宣言された 10pp よりはるかに粗くなるので、`unresolved` の cell と `unresolved` のボトルネックになるでしょう。それは smoke 実行の正直な結果であり、失敗ではありません。verifier cell には各 case の記録済み `*.transcript.jsonl` が必要で、ないものは数えられるのではなく `no_recorded_transcript` としてスキップされます。また、今日の古い premium 基準に対しては、さらに `degenerate_gold` で返ってきます（[verifier cell には混合した gold が必要](#verifier-cell-には混合した-gold) を参照）。

**exit code。** 失敗した、または `unresolved` の cell は*測定*なので、`--matrix` は 0 で終了します。非ゼロで終了するのは、仕様／インフラの失敗、つまり拒否されたフラグの組み合わせ、書き込めないレポート、またはマトリクス全体で使える観測が 1 件もない場合（すべての実行がエラー、スキップ、または差し替えられた場合）だけです。その場合に緑で終了すると、事実と異なることを伝えることになります。

---

## 進化との統合：外部の物差し

eval は、進化エンジン内部の verifier に対する**独立した**対照です。

- 内部の verifier は、モデル*自身*の判断を基準に提案を採点します。自分が採点している振る舞いと一緒にドリフトする可能性があります。
- eval suite は*実際に動いているエージェント*を、エージェントのルールが変わっても動かない**人が書いた期待される振る舞い**と照らし合わせて採点します。あるルールが学習の過程で「必ず返金ポリシーのページを引用する」という振る舞いをこっそり失ってしまったとしても、`must_use_tools` / `output_regex` の case は、内部の verifier がその変更を承認していたとしても赤くなります。

v1.53 以降、この配線は稼働しており、しかも**エントリー単位**です（AEE、つまりデフォルトの進化エンジンです。詳細は
[`docs/architecture/evolution-engine.md`](../../architecture/ja-JP/evolution-engine.md) の第 12 章を参照）：

- すべての playbook エントリーは、作成時に最低 1 件の eval case（G6）にリンクされていなければならず、記録済みの transcript に対して LLM ゼロで再生される E1 アサーション（`G-Assertions` ゲート。transcript が見つからない場合は正直に*未検証*とラベル付けされ、黙って通過することは決してありません）を伴います。
- AEE の Measure ステップは、subprocess として（runtime-agnostic に、決して in-process ではなく）`duduclaw eval … --replay --report` を実行して候補を採点し、その JSON レポートを読み取ります。
- 1 ラウンドが commit された後、各エントリーは `aee_settle_hours` の経過後に**自分自身がリンクしている case** に基づいて個別に確定（確認／ロールバック）します。退行が起きても、原因となったそのエントリーだけがロールバックされます。

ファイル全体を対象にしたレガシー SOUL.md パスの 24 時間観察期間（`ObservationFinalizer` / `duduclaw evolution finalize`）は 2026-09-29（S11）に削除されました。現在残る唯一の観察ウィンドウは、エントリが自分にリンクされた eval case に対して個別に確定するものです。

---

## ファイルの配置

```
evals/                              # あなたの eval suite（repo からの相対パス）
├── examples/
│   ├── greeting-replay.toml        #   オフライン replay のサンプル
│   ├── greeting-replay.transcript.jsonl
│   ├── grounded-replay.toml        #   オフライン replay のサンプル（[[expect.grounded]]）
│   ├── grounded-replay.transcript.jsonl
│   └── refund-flow.toml            #   live のサンプル（エージェントが必要）
└── <suite>/
    ├── <case>.toml
    └── <case>.transcript.jsonl     #   記録済みの基準（--record 経由）
```

実装は `crates/duduclaw-cli/src/eval/` にあります。
`case.rs`（フォーマットと検証）、`transcript.rs`（stream-json の解析）、
`assertions.rs`（決定論的なチェック）、`judge.rs`（LLM ルーブリック。RFC-26 の fork-judge の `LlmCaller` パイプラインを再利用）、`runner.rs`（live の spawn、replay、runtime 汎用の実行）、`stats.rs`（Miller／CLT の統計）、`matrix.rs` と `verifier_cell.rs`（能力マトリクス）、そして
`mod.rs`（全体のオーケストレーションとレポート生成）です。永続化されるマトリクスの型は
`duduclaw_core::role_model_matrix`（`role_model_matrix.toml`）です。
