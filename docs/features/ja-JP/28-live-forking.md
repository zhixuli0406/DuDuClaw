# Live Run Forking（ライブ実行フォーク）

> 進行中の実行を1つからN個の競合ブランチに分割し、AIジャッジに最良のものを残させ、勝者だけをマージし戻す。

---

## たとえ話：迷路の分岐ごとに自分を複製する

迷路の分岐点に立っているところを想像してください。どの道が出口に通じるか分かりません。1つの道を選んで祈る代わりに、**自分を複製します**——1体は左のトンネルへ、1体は右へ、1体はまっすぐ。各分身は独立して、自分の足で、自分の通路で、互いにぶつかることなく探索します。

分身たちが戻ってくると、**入口のジャッジ**が各々に何を見つけたか尋ねます：出口に到達したか？ ルートはどれだけきれいだったか？ 確信があるか？ ジャッジは実際に出口を見つけた分身を残し、残りを捨てます——まるでその行き止まりが最初から存在しなかったかのように。

DuDuClawのLive Run Forkingは、この発想をエージェントの実行に適用したものです。1つのタスクが複数の並列**ブランチ（branch）**に分割され、各ブランチは独自の隔離されたワークスペースで、独自のアカウントと独自の予算を持って実行されます。完了すると、**AIジャッジ**がそれらを採点し、勝者の成果がparentにマージされます——敗れたブランチは破棄されます。

このパターンは [`pydantic-deepagents`](https://github.com/vstorm-co/pydantic-deepagents) に着想を得ています。その看板機能はまさにこれです：進行中の実行を競合ブランチにフォークし、ジャッジに勝者をマージさせる。RFC-26 はこの概念を DuDuClaw 既存の `AccountRotator`、コンテナサンドボックス、GVU ジャッジプリミティブの上に対応付けます。

---

## なぜ単なるリトライではなくフォークなのか

DuDuClawのGVU自己対戦ループは既に答えを改善できます——しかしそれは**逐次的**です：生成、検証、更新、繰り返し。それは単一の推論ラインが一歩ずつ自分を洗練していくものです。

フォークは異なります。それは**真に独立した試行を並列に**実行し、各々が異なる戦略へ誘導される可能性があります：

```
逐次（GVU）：   attempt → critique → attempt' → critique → attempt''
               （単一の推論ライン、時間とともに洗練）

並列（Fork）：  ┌─ branch A：「ステートマシンでリファクタ」
      fork_run ─┼─ branch B：「early returnでリファクタ」
                └─ branch C：「ルックアップテーブルでリファクタ」
                （3つの独立した試行、最後にジャッジが裁定）
```

タスクに複数のもっともらしいアプローチがあり、複数を同時に試す余裕があるとき、フォークは深さではなく幅で解空間を探索します。

---

## フロー

```
                      fork_run(prompt, n, strategies[], budget, merge_mode)
                                   |
                                   v
        ┌──────────────────────────────────────────────────────────┐
        │  各ブランチが取得するもの：                                │
        │   • copy-on-write ワークスペース overlay（read-through）   │
        │   • AccountRotator からの独立アカウント                    │
        │   • 自身の per-branch budget_usd 上限                      │
        │   • 任意の steering メッセージ                             │
        └──────────────────────────────────────────────────────────┘
                                   |
              ┌────────────────────┼────────────────────┐
              v                    v                    v
        ┌──────────┐         ┌──────────┐         ┌──────────┐
        │ branch A │         │ branch B │         │ branch C │   ← tokio::spawn、
        │ (overlay)│         │ (overlay)│         │ (overlay)│     並列実行
        └────┬─────┘         └────┬─────┘         └────┬─────┘
             │  （各ブランチで任意の test_command 実行）│
             └────────────────────┼────────────────────┘
                                   v
                            ┌─────────────┐
                            │  AI ジャッジ │   confidence =
                            │ JudgeAgent  │     quality_spread·0.4
                            └──────┬──────┘   + test_pass_ratio·0.4
                                   │          + internal_consistency·0.2
                                   v
                            ┌─────────────┐
                            │  MergeMode  │   Auto / AutoWithFallback /
                            │   resolve   │   Vote / Manual
                            └──────┬──────┘
                                   v
                  winner.overlay.promote() → parent ワークスペース
                  （敗者は破棄；他は何も変わらない）
```

すべては**デフォルトオフ**です。エージェントが `agent.toml` に `[fork] enabled = true` を設定するまで、フォークは一切発生しません。

---

## プロセス横断の信頼できる情報源：ForkStore

フォークは MCP-server プロセス内で**実行**されます（`claude` サブプロセスがそこで spawn される）が、`/metrics` とダッシュボードの `fork.list / inspect / resolve` RPC を**提供する**のは gateway です。プロセス内レジストリはこの両プロセスにまたがれません。

そこで、ブランチとフォークの状態は共有 WAL SQLite データベース——`<home>/fork_store.db` にある `ForkStore`——に格納され、両プロセスが開きます：

```
        ┌────────────────────────┐        ┌────────────────────────┐
        │   MCP-server プロセス    │        │     gateway プロセス     │
        │  （ブランチ実行を spawn） │        │  （/metrics + dashboard）│
        │                         │        │                         │
        │  mcp_fork /             │        │  render_fork_metrics_from│
        │  mcp_fork_exec          │        │  handle_fork_list/...    │
        └───────────┬────────────┘        └───────────┬─────────────┘
                    │  書き込み                        │  読み取り
                    │            ┌──────────────┐      │
                    └───────────▶│  ForkStore   │◀─────┘
                                 │  (WAL SQLite │
                                 │  fork_store. │
                                 │     db)      │
                                 └──────────────┘
                       WAL + busy_timeout ⇒ 並行読み書き安全
```

`mcp_fork` + `mcp_fork_exec` ハンドラは、旧来のプロセス内レジストリ**から** `ForkStore` **へ**リファクタされました。`forks` と `fork_branches` の2テーブルを持ち、`insert_fork`、`update_branch`、`set_resolution`、`set_all_branch_states`、`get_fork`、`list_branches`、`list_forks`、`metrics` を公開します。`SqliteMemoryEngine` と同じパターン（WAL + busy timeout）で並行読み書きアクセスを安全にします。

---

## ブランチのライフサイクル状態

ブランチは型付き `BranchState` enum を通って遷移します（呼び出し側が数値コードを推測する必要なし）：

| 状態 | 意味 | ジャッジ可？ |
|------|------|------------|
| `Pending` | 作成済み、未開始 | 否 |
| `Running` | サブプロセス実行中 | 否 |
| `Finished` | クリーンに完了——ジャッジ対象 | **可** |
| `BudgetKilled` | per-branch または aggregate 予算上限超過で終了 | 否 |
| `Terminated` | `terminate_branch` による外部終了 | 否 |
| `Failed` | アカウントなし、spawn 失敗、または executor エラー | 否 |

`Finished` ブランチのみ `is_judgeable()` です。ジャッジ可能なブランチが**ゼロ**の場合、ジャッジはエラーを返し、呼び出し側はそのフォークを operator に委ねます——無から自動選択することは決してありません（fail-closed）。

---

## 予算の執行

`budget.rs` には2層の予算保護があり、いずれも fail-closed です：

**`Pool`——完了後の課金計上。** 各ブランチは per-branch 上限とともに `register` され、`try_charge` が per-branch 上限と aggregate 上限の*両方*を執行します。いずれかの拒否（`BranchExceeded` / `AggregateExceeded`）でも何もコミットされません——呼び出し側はそのブランチを停止しなければなりません。未登録のブランチは上限ゼロなので、正の課金はすべて拒否されます。

```
Pool::try_charge(branch, amount)：
     branch_spent + amount > branch_cap   ⇒ BranchExceeded   （コミットなし）
     aggregate_spent + amount > agg_cap    ⇒ AggregateExceeded （コミットなし）
     それ以外                              ⇒ Allowed          （両カウンタをコミット）
```

**`LiveAggregate`——ストリーミング時の先制終了。** ブランチが stream-json をストリームする間、その*累積中*の `total_cost_usd` がライブで監視されます。進行中の合算支出が aggregate 上限を越えた瞬間、`observe` が**単一の最も高価な進行中ブランチ**（id による決定論的タイブレーク）を指名し、呼び出し側がストリーミング途中でそれを終了できるようにします——各ブランチが自身の上限に当たるのを待つのではなく、犠牲を最小化します。NaN／負のコストは 0.0 に浄化され、`finish` はブランチ終了後にその予算を生存者へ解放します。

サイレントな上限はありません：ブランチ数が利用可能なアカウント数に縮小される場合、その縮小はログに記録されます。

---

## ジャッジ

`JudgeAgent` は `JudgeVerdict { winner, confidence, per_branch_scores, rationale }` を生成します。confidence は `JudgeScores` の deep-agents 式に従います：

```
confidence = quality_spread       · 0.4
           + test_pass_ratio      · 0.4
           + internal_consistency · 0.2
```

各サブスコアは `[0,1]` にクランプされます（範囲外 ⇒ クランプ + 警告；NaN ⇒ 0.0）。2つの実装が提供されます：

- **`LlmJudge<C: LlmCaller>`**——複数候補・XML区切りのプロンプト（`build_judge_prompt`）を構築し、候補タグをエスケープして注入耐性を持たせ、応答を JSON 優先（fence 除去付き）で解析します。バックエンドは注入式なので、gateway は Confidence Router（local 優先、信頼度が低いとき Claude にエスカレート）を接続できます。解析不能な判定や範囲外のインデックスはエラーを返します（fail-closed）。
- **`HeuristicJudge`**——決定論的・ゼロ LLM のフォールバックジャッジで、LLM が利用できないときに使われます。

`test_pass_ratio` はジャッジ可能ブランチ全体の `test_exit_code` から算出され、どのブランチもテストされていないときは中立値（0.5）になります。

---

## マージモード

`merge::resolve` は判定を `MergeDecision { winner, needs_confirmation, reason }` に変換します：

| モード | 挙動 |
|--------|------|
| `Auto` | 判定の勝者を選び、即座に promote、人の介入なし |
| `AutoWithFallback` | **デフォルト**——勝者を選ぶが、promote 前に確認を浮上させる |
| `Vote` | ジャッジを `VOTE_ROUNDS`（3）回サンプリングし多数決；同数または平均信頼度が低い ⇒ 保留 |
| `Manual` | 常に operator に委ねる（`winner = None`） |

`DEFAULT_CONFIDENCE_THRESHOLD` を下回る勝者は**モードに関わらず**保留されます。勝者の overlay は、決定が最終で確認不要のときに**のみ** parent ワークスペースへ `promote()` されます——そうでなければ operator が `merge_or_select` で解決するまで parent は手つかずのままです。

---

## Copy-on-Write Overlay

各ブランチは `BranchOverlay` 内で作業し、これは parent ワークスペースを read-through しつつ、promote まで書き込みをローカルに保ちます：

- **`Snapshot`**——可搬な MVP バックエンド：parent の再帰的 `copy_tree`。
- **`NativeCow`**——macOS/APFS では `cp -c` 経由の `clonefile(2)`、Linux btrfs/XFS では `cp --reflink=always`。

`detect_backend()` はホストを一度だけプローブし（キャッシュ）、ネイティブ clone が失敗したら `Snapshot` にフォールバックします——**隔離は決して損なわれず、速度/容量の最適化だけが犠牲になります**。敗れた overlay は破棄され、勝者の書き込みだけがマージされます。

---

## MCP ツール

6つのツールはすべて `Scope::ForkExecute` でゲートされ（明示的に列挙；未知のツールはデフォルトで Admin scope を要求——既存の fail-closed ルール）、エージェントが `[fork] enabled = true` のときのみ使用可能です：

| ツール | 用途 |
|--------|------|
| `fork_run` | 現在のタスクを N 個のブランチに分割 |
| `inspect_branches` | 生存ブランチ + 状態 + 支出を一覧 |
| `diff_branches` | 2ブランチ間のファイル/出力 diff を表示（各側 `truncate_bytes`） |
| `merge_or_select` | フォークを解決——ジャッジ判定または明示的選択 |
| `terminate_branch` | 暴走ブランチを終了（未開始はキャンセル；進行中サブプロセスはストリーミング途中で SIGKILL） |
| `fork_cost` | 合算 + per-branch 支出 |

`fork_run`、`merge_or_select`、`terminate_branch` は監査証跡のため `is_state_changing` にフラグされています。

---

## 設定

`agent.toml` でエージェントごとに設定：

```toml
[fork]
enabled              = false              # デフォルトオフ
max_branches         = 4                  # ハード上限（アカウント/クォータ破綻を回避）
default_budget_usd   = 0.50               # per-branch 上限
aggregate_budget_usd = 1.50               # 全ブランチ横断
merge_mode           = "auto_with_fallback"
test_command         = ""                 # 任意；空 ⇒ test_pass_ratio 中立化
test_timeout_s       = 120
```

欠落または不正な `[fork]` ブロックは無効化された fail-safe です——panic しません。不正なサブ値（例：`max_branches = 0`、負の予算）はデフォルトにフォールバックし、未知の `merge_mode` 文字列は警告付きで `AutoWithFallback` にフォールバックします。

---

## 可観測性

gateway の `/metrics` エンドポイントはスクレイプ時にプロセス横断の `ForkStore` を読み、Prometheus 行を出力します：

| メトリクス | 型 | 意味 |
|-----------|-----|------|
| `duduclaw_fork_runs_total` | counter | 作成されたフォークの総数 |
| `duduclaw_fork_resolved_total` | counter | 勝者に解決されたフォーク |
| `duduclaw_fork_promoted_total` | counter | 勝者が promote されたフォーク |
| `duduclaw_fork_branches_total` | counter | 全フォーク横断のブランチ総数 |
| `duduclaw_fork_branch_outcome{outcome="finished\|budget_killed\|failed"}` | counter | 終端結果別のブランチ |
| `duduclaw_fork_spend_usd_total` | counter | 全フォーク横断の合算 USD 支出 |

各フォーク解決は `with_file_lock`（プロセス横断で安全）経由で `<home>/fork_history.jsonl` にも追記され、gateway の `activity` テーブルに `fork_resolved` 行を挿入してダッシュボードの Activity Feed に表示されます。ダッシュボードの `ForkPage` は最近のフォークを一覧し、ブランチを並べて表示し、ジャッジの勝者を強調し、手動解決を提供します（`fork.resolve` は manager チェックで保護）。

---

## 実装済み vs 計画中

RFC-26 §5 に従い、6つの全フェーズ（P1–P6）が着地しています：

| フェーズ | 範囲 | 状態 |
|---------|------|------|
| P1 | `duduclaw-fork` crate：`Branch`、`BranchOverlay`、`budget::Pool`、controller | 完了 |
| P2 | `JudgeAgent` + `JudgeVerdict`、test runner、merge modes | 完了 |
| P3 | 6 MCP ツール + `Scope::ForkExecute` + dispatch 配線 | 完了 |
| P4 | 並列実行、aggregate 予算プール、native CoW、ストリーミング予算終了、外部 SIGKILL | 完了 |
| P5 | 共有 `ForkStore` 経由の Prometheus `/metrics` + `fork_history.jsonl` + Activity Feed + dashboard `ForkPage` | 完了 |
| P6 | 副次的なパリティ項目（Plan Mode、checkpoint fork/rewind + SQLite、built-in skills、memory `/improve`、Task Board claim + サイクル検出） | 完了 |

**唯一の設計上の除外**：`terminate_branch` は、それを所有するプロセス内の子のみを終了できます。*別の*プロセスをまたいでブランチサブプロセスを終了すること（例：gateway が MCP-server の子に手を伸ばす）は範囲外です——終了はその子が spawn された場所から発する必要があります。

---

## なぜ重要か

### 幅優先の問題解決

タスクによっては、もっともらしい戦略が複数あり、事前にどれが最良か明らかでないことがあります。フォークはそれらを同時に試し、証拠（テスト + ジャッジ）に裁定させます——1つのアプローチに賭けて1時間後にそれが誤りだったと気づくのではなく。

### レート制限の衝突なし

各ブランチが `AccountRotator` から**独立したアカウント**を引くため、N 個の並列実行が単一アカウントのレート制限を奪い合うことはありません。ブランチ数は利用可能アカウント数に縮小されるので、独立性は常に保たれます。

### コストは有界に保たれる

2層の予算——per-branch とライブ aggregate——により、暴走ブランチは予算全体を燃やし尽くす前にストリーミング途中で終了されます。何も静かに上限されることはなく、縮小は記録されます。

### 全体を通じて Fail-Closed

ジャッジの欠如、解析不能な判定、サンドボックス spawn 失敗、ジャッジ可能ブランチがゼロ——これらすべては operator に委ねられ、静かな自動選択は決してありません。閾値未満の信頼度も保留されます。デフォルトオフの姿勢は、明示的に有効化されるまでこれらが一切動作しないことを意味します。

---

## まとめ

迷路で迷い、かつ自分を複製する余裕があるとき、1つのトンネルを選んで祈ったりはしません——各トンネルに分身を送り、入口のジャッジに出口を見つけた分身だけを残させます。Live Run Forking は DuDuClaw エージェントにその能力を与えます：並列の競合ブランチ、隔離されたワークスペース、独立したアカウント、有界の予算、AI ジャッジ、そして gateway とダッシュボードがそれを見守れるようにするプロセス横断の `ForkStore`。デフォルトオフ、fail-closed、サイレント上限なし——幅で探索し、勝者だけをマージする。
