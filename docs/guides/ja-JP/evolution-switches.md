# 進化スイッチ一覧：各トグルが制御するもの

DuDuClawのエージェントは時間とともに自己改善できます。予測誤差を振り返り、playbook の経験則を蓄積し（`SOUL.md` はエージェント自身からは読み取り専用です）、新しいスキルを合成し、活用されていない領域を探索します。これらの経路にはそれぞれ専用のスイッチがあります。本ガイドは、どのスイッチが何を管轄するか、そしてエージェントを完全に凍結する方法をまとめた唯一のマップです。

## マスタースイッチ

`agent.toml`：

```toml
[evolution]
enabled = true   # master kill-switch (default: true)
```

`enabled = false`にすると、**そのエージェント上のあらゆる自律的な進化経路が無効化**されます。下記の個別トグルの設定に関係なく効きます。エージェントに自己変更をやめさせたいときに切るべき唯一のスイッチです。デフォルトは`true`なので、このフィールドが存在する前に作成されたエージェントは、これまでどおりの挙動を維持します。

具体的には、`enabled = false`のとき：

| 経路 | 何が止まるか |
|---|---|
| 進化ループ（AEE） | playbookのラウンドは実行されず、確定観察ウィンドウも開かれない |
| Heartbeatの沈黙ブレーカー | 沈黙後の強制リフレクションを**発火しない** |
| チャンネル予測経路 | スキルの診断／有効化／合成／卒業とGVUトリガーがすべてスキップされる |
| サブエージェントディスパッチのリフレクション | `maybe_run_gvu`が即座に短絡して戻る |
| スキル合成の自動実行スケジューラ | グローバルに有効でも、凍結された対象エージェントはスキップされる |

予測誤差の**ロギング**は引き続き動作します。これは受動的な観測（テレメトリ）であり自己改変ではないため、ダッシュボードの数値は正確なまま保たれます。

## 機能ごとのトグル

マスタースイッチの下に、各機能ごとの個別フラグがあります。以下のうち少なくとも1つがオンであれば、`is_any_evolution_enabled()`はtrueになります。

| トグル | デフォルト | 制御対象 |
|---|---|---|
| `gvu_enabled` | **出荷時から `true`**（下記参照） | AEE playbook 進化ループ |
| `strategy` | `"balanced"` | AEE のラウンド意図配分：`balanced` / `innovate` / `harden` / `repair_only` |
| `skill_synthesis_enabled` | `false` | 繰り返し発生するドメインギャップから新しいスキルを合成する |
| `skill_auto_activate` | `false` | 会話の途中で提案されたスキルを有効化する |

**`gvu_enabled` は 2026-09-29（K2）から出荷時 `true` です。** それを安全にした変更が2つあります。第一に、エンジンはもう `SOUL.md` を書き換えません——レガシー経路は同日（S11）に削除され、進化する対象は playbook（少なくとも1つの eval ケースに紐づいた、独立して引退可能な小さな行動ルール）になりました。`SOUL.md` はエージェントに対して読み取り専用のままです。第二に、1ラウンドのコストは両側から抑えられています：`gvu_cooldown_minutes` がラウンドの開始間隔を制限し、LLM コストゼロの Gate が judge 呼び出しを支払う前に見込みのない候補を却下します。

値はすべての scaffold が明示的に書き込むため、トグルは `agent.toml` 上で常に見えます。既存の `agent.toml` は現状の値を保ちます（キーが無ければ `false` と読まれ、ランタイム側のフェイルクローズなゲートは変わっていません）。オプトアウトするには `gvu_enabled = false` を設定してください。

同日（H3）、4 つのスキル系ノブが実際に効くようになりました。それまではダッシュボードが書き込むだけで、読み手がいませんでした：

| ノブ | デフォルト | 制御対象 |
|---|---|---|
| `max_active_skills` | `5` | 同時に有効なスキル数の上限（超過時は最も成績の悪いものを退避） |
| `skill_synthesis_threshold` | `3` | 合成シグナルが発火するまでのドメインギャップ反復回数 |
| `skill_synthesis_cooldown_hours` | `24` | 同一トピックで発火した後の静穏期間 |
| `skill_graduation_min_lift` | `0.1` | 卒業候補になるための最小実測リフト |

同族の8キー（`skill_graduation_enabled`、`skill_recommendation_enabled`、`skill_recommendation_threshold`、`curiosity_enabled`、`curiosity_threshold`、`curiosity_max_daily`、`skill_behavior_monitor_enabled`、`skill_behavior_drift_threshold`）は読み手が皆無だったため削除しました。既存の `agent.toml` に残っていても無害で、無視されます——それは元々の実際の挙動そのものです。

### GVUクールダウン

上記のトグルとは独立して、すべてのGVUトリガー経路（チャンネル返信のε-exploration、サイレンスタイマー、サブエージェントディスパッチの強制リフレクション）は単一のエージェントごとのクールダウンを共有します。これにより、トリガーが連続発生しても数分がかりのGVUサイクルが立て続けに連鎖することを防ぎます。

```toml
[evolution]
gvu_cooldown_minutes = 60   # default 60; 0 disables the cooldown
```

クールダウンはトリガーがゲートを通過した瞬間からカウントを開始します（サイクル終了時ではありません）。結果（applied／abandoned／skipped）にかかわらず適用されます。スロットリングの対象は成功した呼び出しだけでなく、*試行された*LLM呼び出しのコストだからです。状態はメモリ上にあり、gatewayの再起動でリセットされます。

### 障害帰属（`config.toml [evolution] fault_attribution`、デフォルト `true`）

却下されたgoal-loopのラウンドが、注入済みのplaybookルールに対してカウントされる前（`harmful`の加算、シャドウ候補のスコアリング、F2bの統合ソース）に、gatewayは決定的でLLMコストゼロのルールチェーンでそのラウンドの障害側を分類します（`crates/duduclaw-gateway/src/fault_attribution.rs`）。`grader`（判定が決定的なgrounding証拠と矛盾した）、`environment`（rate limit／課金／タイムアウト／spawn失敗）、`harness`（返信がツール使用を主張しているのにネイティブのツールイベントが観測されなかった、またはケイパビリティゲートが呼び出しをブロックした）、`unknown`（観測の忠実度がない）、それ以外は`model`です。学習ループに入るのは`model`のラウンドだけで、除外されたラウンドはすべて`fault_attributed`監査イベントを書き込むため、除外が見える形で残ります。`fault_attribution = false`にすると、2026-09以前の挙動をバイト単位で復元します。根拠：arXiv:2605.22842（帰属システムが64/64件の失敗でモデルを責めた）およびarXiv:2607.28802。

### どのエンジンが動くか：AEE のみ

`gvu_enabled = true`のとき、動作する進化エンジンは**AEE**（Agentic Evolution Engine）です。AEEはエージェントの*playbook*、つまり少なくとも1つのevalケースに紐づいた、独立して引退可能な小さな行動ルール群を進化させます。`SOUL.md`を書き換えることは一切ありません。ペルソナファイルの所有権はオペレーターにあります。

**2026-09-29（S11）に削除：`legacy_soul_evolution` エスケープハッチと、その背後にあった `SOUL.md` 全文書き換えの Generator→Verifier→Updater サイクル**、あわせて `SOUL.md` のバージョン管理、24時間の観察ウィンドウ、自動ロールバック、サイズ上限の統合ブレーカー、`duduclaw evolution finalize` CLI も削除しました。`SOUL.md` は Evolution v3（WP1.1）以降エージェントに対して読み取り専用であり、これらが守っていた書き込み経路自体がすでに存在しませんでした。既存の `agent.toml` に `legacy_soul_evolution = true` が残っていても無視され、そのエージェントは他と同じく AEE 経路を通ります。

AEEのラウンドがコミットされた後、追加されたエントリは判定が確定する前に観察されます。

```toml
[evolution]
aee_settle_hours = 24   # default 24; the agent runs no new AEE round until it elapses
```

### 戦略ミックス（AEEが各ラウンドでどの意図を選ぶか）

各AEEラウンドは、以前の生のε-explorationに代わり、エージェントごとのミックスから決定的に1つの意図を選びます。`repair`（`MistakeNotebook`の積み残しを消化）、`optimize`（既存の`success_streak`が低いエントリを改善）、`innovate`（新しいエントリを提案）のいずれかです。

```toml
[evolution]
strategy = "balanced"   # balanced (default) | innovate | harden | repair_only
```

| `strategy` | Repair（修復） | Optimize（最適化） | Innovate（革新） |
|---|---|---|---|
| `balanced` | 5 | 3 | 2 |
| `innovate` | 2 | 3 | 5 |
| `harden` | 4 | 5 | 1 |
| `repair_only` | 10 | 0 | 0 |

認識できない値は`warn!`を出し、`balanced`にフォールバックします。タイプミスが進化の挙動を黙って変えてしまってはいけません。ミスの積み残しが空のときは、設定されたミックスにかかわらず`repair`は`optimize`側に絞られます。

コミットゲートの次元ごとのノイズバンド（チャンピオンとどれだけ近ければ「引き分け」つまりmatches-or-improvesにおける「matches」とみなすか）も設定可能です。デフォルト値は経験的な較正待ちの初期値であり、調整済みの数値ではありません。

```toml
[evolution.noise_band]
cases = 0.05     # eval-case pass-rate dimension; hard-clamped to ≤ 0.10
                 # (a wider band means the cases are noisy, not that the band should widen)
judge = 0.15     # LLM judge score dimension (judges vary run to run)
anti_sycophancy = 0.0   # deterministic — zero band
novelty = 0.05
relevance = 0.10
```

### Evalコーパスの場所（AEEの計測基盤）

AEEはエージェントのeval suiteをリプレイして候補を採点します。あるエージェントに対応するsuiteは、suites root配下にそのエージェント名を冠したディレクトリです。

```toml
# ~/.duduclaw/config.toml
[evolution]
eval_suites_root = "evals"        # default: <home>/evals; relative paths resolve against the home dir
# eval_binary    = "/usr/local/bin/duduclaw"   # optional: which binary to spawn for `duduclaw eval`
```

`DUDUCLAW_EVAL_SUITES_ROOT`は1プロセス限定で`eval_suites_root`を上書きします。開発者のチェックアウトでは通常、リポジトリの`commercial/evals`を指すよう設定されます。

**スコアが意味を持つには、コーパスを一度記録しておく必要があります。** AEEはリプレイモード（オフライン、LLMコストゼロ）で計測を行い、各ケースにつき記録済みのtranscriptを読み込みます。`<stem>.transcript.jsonl`が横に存在しないケースはリプレイできません。

```bash
duduclaw eval ~/.duduclaw/evals/<agent-id> --record   # one live pass, then replay is free
```

その記録が存在するまで、suite全体は失敗としてではなく*未計測*として扱われます。記録されていないケースは品質シグナルではなくインフラ上のギャップであり、それを0.0点と採点してしまうと、誰にも改善できないゼロだらけのチャンピオンを固定化してしまいます。

**suiteがなくても計測は適切に劣化します。** コーパスが未記録（またはeval binaryに到達できない）エージェントも計測自体は行われます。`cases`次元は決してゼロではなく*欠落*として報告され、コミットゲートは実際に存在する次元のみを比較します。この劣化はそのラウンドの監査記録（`case_dimension_available: false`）と`warn!`のログ行に可視化され、サイレントには起こりません。

**ただし新規エントリには少なくとも1つのevalケースが必須です（v1.53、G6/E1）。** すべてのplaybook `Add`は1つ以上のevalケースに紐づき、機械的にチェック可能なアサーション（`must_use_tools` / `output_contains` / …）を持たなければなりません。evalケースがゼロのエージェントは*新規*ルールを蓄積できません。エージェントのSOUL行動ルールからコーパスをブートストラップするには：

```bash
duduclaw eval-scaffold --agent <agent-id>   # drafts into evals-drafts/
```

ドラフトをレビューし、良いものを`evals/<agent-id>/`に移してから、上記の方法で記録してください。ドラフトは意図的に独立した`evals-drafts/`ディレクトリに書き出されるため、未レビューのケースが本番コーパスに漏れ込むことはありません。記録済みtranscriptのないケースに対するアサーションのリプレイは*未検証*（参考情報）として報告され、サイレントに合格扱いになることはありません。

v1.53以降、記録処理は副作用がありません。`--record`はエージェントの`.mcp.json`を一時コピーに書き換え、その`DUDUCLAW_HOME`はeval home（およびプレースホルダーのMCPキー）を指すため、記録実行が本番状態に触れたり、transcriptに実際のキーを漏らしたりすることはありません。

## Autopilotはマスタースイッチによって意図的に管轄されない

Autopilotルール（`autopilot.*`）は**ユーザーが明示的に設定した自動化**です。あなた自身が書いたルールなので、DuDuClawはそれを命令として扱い、エージェントが自律的に進化させたものとは見なしません。進化のマスタースイッチはautopilotに一切触れません。特定のautopilotルールを止めたい場合は、ダッシュボードのAutopilotページで無効化してください。

唯一の例外は下記の緊急凍結です。これは乱暴に「すべて止める」ためのものであり、autopilotは別途無効化するようリマインドされます。

## ワンショットの凍結／解凍（エンタープライズ向けエスケープハッチ）

何かがおかしいと感じ、エージェントに*今すぐ*自己変更をやめさせたいときは：

```bash
duduclaw agent freeze <agent-id>
```

これは1回の編集で`[evolution] enabled = false`と`[heartbeat] enabled = false`の両方を設定し、`security_audit.jsonl`に1件のレコード（`event_type = agent_freeze`）を書き込みます。何も削除されません。元に戻すには：

```bash
duduclaw agent unfreeze <agent-id>
```

これで`[evolution] enabled = true`と`[heartbeat] enabled = true`が復元されます。Autopilotルールは自動的には変更されません。必要であればダッシュボードから無効化するよう、このコマンドがリマインダーを表示します。

## 凍結が実際に効いていることを検証する

マスタースイッチの要点は、切り替えた後に何も進化していないことを証明できる点にあります。確認方法：

1. エージェントに`[evolution] enabled = false`を設定する。
2. `evolution.db`（`gvu_experiment_log`）と`prediction.db`（`evolution_events`）を観察する。新しい行が現れないはずです。
3. `SOUL.md`は変化しないはずです——もっとも2026-09-29以降、プラットフォームにはエージェントに代わってそれを書く経路自体がありません。
4. 確定観察ウィンドウが開かれないはずです（`aee_pending_settlement` に保留行がない）。

これは本プロジェクトがこの機能に対して実行している自動検証と対応しています。

## 他ページにある関連スイッチ（v1.53）

進化トグルそのものではありませんが、同じ「学習と検証」の一部です。

| キー | デフォルト | ページ |
|---|---|---|
| `config.toml [memory] novelty_gate` | `true` | [memory-and-knowledge.md](./memory-and-knowledge.md) — ほぼ重複した意味記憶を拒否する |
| `config.toml [dispatch] grounding_precheck_enabled` | `true` | [goal-loop.md](./goal-loop.md) — 承認判定の前にLLMコストゼロで証拠をチェックする |
| `config.toml [dispatch] two_stage_judge` | `true` | [goal-loop.md](./goal-loop.md) — MAV承認パネルの前に低コストな第一段階の評価器を挟む |
| `config.toml [goal_loop] resume_on_restart` | `"pause"` | [goal-loop.md](./goal-loop.md) — gateway再起動時に進行中のゴールタスクを`needs_human`にエスカレーションする。代わりに再開させるには`"auto"`を設定。ダッシュボード：設定 → 自動化 |
| `config.toml [task_forward_model] enabled` | `true`（v1.54 以降） | [goal-loop.md](./goal-loop.md) — タスクレベルの予測・実行・検証のワールドモデル。無効にするには `false` を設定 |
| `config.toml [goal_loop] progress_report_minutes` | `10` | [goal-loop.md](./goal-loop.md) — クレーム済みのゴールタスクがこの分数だけ進捗シグナルを示していないとき通知する（介入はしない）。`0`で無効化 |
| `config.toml [goal_loop] tool_streak_advisory` | `true` | [goal-loop.md](./goal-loop.md) — 1ラウンド内で同一ツール呼び出しが3/5/8回連続したとき、段階的に強まるアドバイザリを注入する。LLMコストゼロで、決してブロックしない |
| `config.toml [dispatch] admission` | `"queue"` | [goal-loop.md](./goal-loop.md) — 容量超過の一時的なサブエージェント生成は即座に失敗する代わりに永続的なFIFOキューに入る。pre-H19の即時拒否挙動に戻すには`"fail"`を設定 |
