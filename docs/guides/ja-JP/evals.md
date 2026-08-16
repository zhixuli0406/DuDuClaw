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
| `--record` | 一度 live で実行し、生の stream-json を各 case の隣に `*.transcript.jsonl` の基準ファイルとして書き出します。以降の `--replay` で使えます。 |
| `--no-judge` | case が `[judge]` を有効にしていてもスキップします（完全に決定論的、コストゼロ）。 |
| `--report <path>` | JSON レポートを書き出します（各 case のアサーション結果、judge のスコア／理由、transcript の診断、所要時間）。 |

**case の id と suite の一意性。** 各 case の安定した id は、そのファイル名の幹です（`[case] name` は人が読むためのタイトルのままで、id ではありません。`--filter` は `name` を、`--case` は id を対象にマッチします）。同じ実行の中で 2 つの case ファイルが同じファイル名の幹を共有している場合、suite は読み込み時点で即座に失敗します。id が黙って衝突すると `--case` が曖昧になってしまうからです。

**exit code：** 1 つでも case が失敗すれば、プロセス全体が**非ゼロ**の exit code を返すので、そのまま CI ゲートに組み込めます。コンソールには人が読める表形式が出力されます。`--report` ファイルはその機械可読版で、既存の詳細な `cases` 配列に加えて、`{suite, total, passed, per_case: [{id, name, passed, failed_assertions, judge_score, mast_class}]}` という簡潔な構造も持つようになりました。gateway の `eval_runner` のようなプログラムからの利用者向けです。

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

## 進化との統合：外部の物差し

eval は、進化エンジン内部の verifier に対する**独立した**対照です。

- 内部の verifier は、モデル*自身*の判断を基準に提案を採点します。自分が採点している振る舞いと一緒にドリフトする可能性があります。
- eval suite は*実際に動いているエージェント*を、エージェントのルールが変わっても動かない**人が書いた期待される振る舞い**と照らし合わせて採点します。あるルールが学習の過程で「必ず返金ポリシーのページを引用する」という振る舞いをこっそり失ってしまったとしても、`must_use_tools` / `output_regex` の case は、内部の verifier がその変更を承認していたとしても赤くなります。

v1.53 以降、この配線は稼働しており、しかも**エントリー単位**です（AEE、つまりデフォルトの進化エンジンです。詳細は
[`docs/architecture/evolution-engine.md`](../../architecture/evolution-engine.md) の第 12 章を参照）：

- すべての playbook エントリーは、作成時に最低 1 件の eval case（G6）にリンクされていなければならず、記録済みの transcript に対して LLM ゼロで再生される E1 アサーション（`G-Assertions` ゲート。transcript が見つからない場合は正直に*未検証*とラベル付けされ、黙って通過することは決してありません）を伴います。
- AEE の Measure ステップは、subprocess として（runtime-agnostic に、決して in-process ではなく）`duduclaw eval … --replay --report` を実行して候補を採点し、その JSON レポートを読み取ります。
- 1 ラウンドが commit された後、各エントリーは `aee_settle_hours` の経過後に**自分自身がリンクしている case** に基づいて個別に確定（確認／ロールバック）します。退行が起きても、原因となったそのエントリーだけがロールバックされます。

レガシーの SOUL.md パス（`[evolution] legacy_soul_evolution = true` でオプトイン）は、引き続きファイル全体を対象にした 24 時間の観察期間（`ObservationFinalizer` / `duduclaw evolution finalize`）を使用します。その事後指標は `prediction.db` と `feedback.jsonl` から得られ、この eval の配線には接続されていません。

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
`assertions.rs`（決定論的なチェック）、`judge.rs`（LLM ルーブリック。RFC-26 の fork-judge の `LlmCaller` パイプラインを再利用）、`runner.rs`（live の spawn と replay）、そして
`mod.rs`（全体のオーケストレーションとレポート生成）です。
