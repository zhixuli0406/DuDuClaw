# 半自動トポロジー進化（D5、human-gated）

DuDuClaw の自律進化（GVU / AEE。v3 以降はデフォルトで AEE playbook エントリを
使い、SOUL.md 全体を書き換える方式ではなくなっています。詳細は
`docs/architecture/evolution-engine.md` の第 12 章を参照）が最適化するのは
「ノード」、つまり各 agent の prompt / 振る舞いルールだけです。agent 間の
「エッジ」、すなわち `reports_to` 階層があるタスククラスを誰にルーティングする
かは、これまでずっとハードコードされてきました。D5 はこのエッジを進化可能な
対象に変えますが、変更のたびに必ず人間の承認を通す必要があり、機械が担当する
のは提案と証拠収集だけです。

設計の系譜：GPTSwarm（arXiv:2402.16823、トポロジーを学習可能なオブジェクトとして
扱う）、AFlow（2410.10762）、ADAS（2408.08435。制御フローの完全自動書き換えは
runaway リスクが最も高い能力であり、D5 が意図的に完全自動化を避けている理由でも
ある）。

> デフォルトは無効です。この仕組み全体は `config.toml` で
> `[topology_evolution] enabled = true` を設定した場合にのみ動作します。無効時の
> 派工パスは、純粋な `FixedHierarchy` と完全に同一（byte-identical）です。

## 動作の流れ

1. **証拠分析**（純粋関数、単体テスト可能）
   バックグラウンドドライバーが `tick_secs` ごとに task store をスキャンし、直近
   `lookback_days` 日間の各 `(agent, task_class)` の品質シグナルを集計します：
   MAV/review の却下率、needs_human へのエスカレーション率、goal-loop の
   無進捗（oscillation）回数。`task_class` はタスクの最初の tag を採用し（D4 の
   RoundRobin と同じ基準）、tag がない場合は priority にフォールバックします。
   サンプルの母集団は「確定済みの goal-mode タスク」（状態が done / needs_human
   / failed）で、needs_human・failed、または `retry_count > 0` はそれぞれ 1 回の
   却下としてカウントされます。

2. **提案**（直接の変更ではない）
   あるタスククラスについて、あるエージェントのサンプル数が `min_samples` 以上
   かつ却下率が `reject_rate_threshold` 以上であり、同じ `reports_to` 親の下にいる
   sibling の誰かが同じタスククラスをより良くこなしている（却下率が低く、
   サンプル数も十分にある）場合、ドライバーは 1 件の `reroute` 提案を生成し、
   証拠（サンプル数、却下率、最大 10 件の sample task id）を添付します。条件を
   満たす sibling がいなければ提案は行いません。空の結果は偽の結果に勝るから
   です。1 tick あたり最大 1 件の提案。

3. **人間のゲート（迂回不可）**
   すべての提案は `ApprovalBroker`（`action_kind = "topology_reroute"`）を経由
   します。これは ActionGuard の意味論では **always-human** に相当し、LLM
   judge は介在せず、`autonomy_level` による緩和も受けません。コード上は
   `request` + `poll` しか呼ばれず、自動承認の経路は一切存在しません。TTL 切れ
   は DENY として扱われます（broker は fail-closed）。人間は dashboard の
   `approvals.decide` またはチャンネルのボタンで承認・却下できます。

4. **反映と観察期間**
   承認されると `~/.duduclaw/routing_overrides.json` に書き込まれます
   （advisory lock ＋ アトミックな temp/rename）。`FixedHierarchy` は派工時に
   まず active な override を確認し、`(task_class, from_agent)` に一致すれば
   `to_agent` に振り替えます。override ファイルが存在しない、または壊れている
   場合は常に override なしとみなされ、ルーティングは現状に戻ります
   （fail-safe）。反映後は `observe_hours`（デフォルト 24h）の観察期間に入ります。

5. **自動ロールバック**
   観察期間中に `to_agent` の当該タスククラスにおける却下率が `from_agent` の
   過去の基準値以上になった場合、override は直ちに `rolled_back` となり、
   ルーティングは自動的に元に戻ります。観察期間を通過し、実際に基準値を上回れば
   `confirmed` になります。サンプル不足の場合は観察期間を 1 回だけ延長し、それ
   でも不足していれば `rolled_back` になります（保守的な収束）。

6. **提案の乱発防止**
   同一の `(task_class, from_agent)` は `proposal_cooldown_days`（デフォルト
   7 日）以内では最大 1 件しか提案できません（却下されたものを含む）。これは
   override ファイルの proposal log に記録されます。active な override や
   pending の提案がすでにある場合も重複提案はされません。`dispatch_guard` の
   スライディングウィンドウも通常どおり適用され、D5 によって迂回されることは
   ありません。

すべての提案／承認／ロールバック／確認は `events.db` のイベントと dashboard の
Activity Feed に書き込まれます（`topology.proposed` / `topology.approved` /
`topology.rejected` / `topology.rolled_back` / `topology.confirmed` /
`topology.extended`）。

## 設定

```toml
[topology_evolution]
enabled = false            # 主スイッチ、デフォルトは無効
lookback_days = 14         # 証拠の遡及ウィンドウ（日）
min_samples = 5            # (agent, task_class) セルに必要な確定済みサンプルの最小数
reject_rate_threshold = 0.6  # 提案をトリガーする却下率の閾値
observe_hours = 24         # 承認後の観察期間（時間）
proposal_cooldown_days = 7 # 同一エッジの提案クールダウン日数
tick_secs = 3600           # ドライバーの tick 周期（秒）
approval_ttl_secs = 86400  # reroute 承認の TTL（秒）、期限切れ＝却下
```

## Dashboard RPC

`topology.list`（require_manager）は、現在の routing overrides と pending の
reroute 提案を返し、dashboard が D5 の状態を表示するために使います。承認／却下
は既存の `approvals.list` / `approvals.decide` をそのまま利用します。

## リスクと境界

- **デフォルトで無効**であり、起動には `ApprovalBroker` が利用可能である必要が
  あります。利用できない場合 D5 は起動しません（人間のゲートを持たない提案
  メカニズムの存在は許容されません）。
- 機械が行うのは可逆な操作（提案、観察、ロールバック）のみで、不可逆な操作
  （実際にルートを変更すること）は常に人間の判断に委ねられます。
- D5 はデフォルトの `FixedHierarchy` 階層ルーティングの上にのみ重なります。
  オペレーターが明示的に `RoundRobin` / `LlmSelect`（`[dispatch] policy`）を
  選んでいる場合、それは意図的なルーティング選択であり、D5 の override は
  そのポリシーの roster が空になって hierarchy にフォールバックしたときにしか
  効きません。
- override はルーティング層の変更であり、実行中のタスクに遡って適用される
  ことはありません。すでに振り分けられた in-flight のタスクは元のルートのまま
  進み、新しいルート（またはロールバック）は今後の派工にのみ適用されます。

opus-playbook の観察期間規律に従い、D5 は D1–D4 が安定し、eval のサンプル数が
十分に集まってから有効化すべきです。
