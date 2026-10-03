# 非推奨と削除

このページは、名前が変わった、または統合された公開名称について、すでに削除されたもの
と、まだ非推奨期間にあるものを一覧にしています。

**方針**: 非推奨の名称は **マイナーバージョン 2 つ分** 残り、その後に削除されます。
方針に変更はありません。v1.69.0 では、v1.66.0 で非推奨になった項目をすべて削除しました。
例外は [引き続き非推奨](#引き続き非推奨) と
[非推奨リストから外した名称](#非推奨リストから外した名称) に記載しています。

非推奨期間中の、各面での示し方:

| 面 | 示し方 | まだ使える？ |
|---|---|---|
| MCP ツール | `description` の先頭に `[deprecated → <新ツール> <パラメータ>]`、`tool_catalog` で `deprecated: true` | 使える。`tools/list` に残り呼び出しも可能。ツールを隠すと呼び出せなくなり、非推奨期間の目的に反するため |
| CLI サブコマンド | clap の `hide = true`。`--help` には出ないが解析される | 使える |
| `config.toml` の値 | 読み込み時にプロセスごとに `warn!` を 1 回、書き込み時は監査イベントも記録 | 使える。設定された動作が黙って置き換えられることはない |
| ダッシュボード | 新しい名称だけを提示する。保存済みの旧い値は「非推奨」ラベル付きで表示 | 使える |
| エージェントランタイム | 読み込み時にプロセスごとに `warn!` を 1 回。ダッシュボード経由の書き込みでは `runtime_provider_deprecated` の監査イベントも記録。ダッシュボードは提示をやめ、保存済みの値に「非推奨」ラベルを付ける | 使える。これまでどおり解析・実行される |

---

## v1.69.0 で削除したもの

### MCP ツール

8 つのツール名は宣言されなくなり、`tools/list` に出ません。呼び出すと、置き換え先の
呼び出し方を示すツールエラーが返ります。ツールの総数は 249 から 241 になりました。

| 削除されたもの | 代わりに使うもの |
|---|---|
| `shared_wiki_ls` | `wiki_ls`（`scope="shared"` を付ける） |
| `shared_wiki_read` | `wiki_read`（`scope="shared"` を付ける） |
| `shared_wiki_write` | `wiki_write`（`scope="shared"` を付ける） |
| `shared_wiki_search` | `wiki_search`（`scope="shared"` を付ける） |
| `shared_wiki_stats` | `wiki_stats`（`scope="shared"` を付ける） |
| `shared_wiki_lint` | `wiki_lint`（`scope="shared"` を付ける） |
| `schedule_task` | `tasks_create`（`schedule="<cron 式>"` を付ける） |
| `skill_bank_search` | `skill_search`（`source="bank"` を付ける） |

`shared_wiki_delete` と `wiki_share` はもともとエイリアスではありません。名前は
そのままで、影響を受けません。

**削除されたツール名を従業員がまだ使っている場合**: `agent.toml [capabilities]` の
ツール一覧、プロンプト、スキルに旧い名前が残っていることがあります。旧い名前の効果は
一覧ごとに異なります。

| 旧い名前を書いている場所 | アップグレード後の効果 |
|---|---|
| `allowed_tools` | この項目はどのツールにも一致しなくなる。旧い名前だけを許可リストに載せている従業員は、その能力を失う。新しい名前に置き換える |
| `denied_tools` | MCP のゲートは、対応する新しい呼び方（例: `scope="shared"` を付けた `wiki_write`）を引き続き拒否する。Claude CLI のフラグは一致しなくなるので、すべての場所で拒否するには新しい名前を書く |
| `approval_required_tools`、`irreversible_tools`、`maybe_irreversible_tools` | ゲートは対応する新しい呼び方に引き続き適用される。`scope="shared"` を付けない `wiki_write` は影響を受けない |
| `scoped_tools` | 対応する新しい呼び方には、引き続きタスク単位の付与が必要。付与は一覧に書かれた名前で申請され、記録される |
| `config.toml [provenance] sensitive_tools` | ゲートウェイは、この新しい名前を引き続き代わりに保護する |

`allowed_tools` 以外では、旧い名前は引き続き保護として働きますが、名前が古くなって
いるため、新しい名前に置き換えることを勧めます。`duduclaw doctor` には削除済みの
ツール名を探す検査があります。対象は、各従業員の `agent.toml [capabilities]` の一覧、
従業員ディレクトリ直下のプロンプトファイル（`SOUL.md`、`IDENTITY.md`、`CLAUDE.md`、
`AGENTS.md`、`GEMINI.md`、`CONTRACT.toml`）、`SKILLS/` と `wiki/` 以下の Markdown、
`config.toml` の `[provenance]` と `[[ccr.allowed_sources]]` です。スケジュール済み
タスクや自動化ルール内のプロンプト文、`evals/` と playbook のツールアサーション、
`.mcp.json`、共有ナレッジベースは検査の対象外です。

統合された 3 つの入口は、これまでと同じ動作です。

- **wiki**: `wiki_*` は `scope: "agent" | "shared"` を受け取り、既定は `agent` です。
  既存の `wiki_*` 呼び出しは変わりません。`.scope.toml` の名前空間ポリシー、
  `wiki_visible_to` の可視性、削除時の「作成者本人またはメインエージェントのみ」の
  ルールも変わっていません。
- **作業の作成**: `tasks_create` は `kind`（既定の `"task"`、または `"goal"`）と
  `schedule`（5 または 6 フィールドの cron 式は定期作業、`2026-10-01T09:00:00+08:00`
  のような RFC3339 時刻は 1 回限りの起動）を受け取ります。どちらの形式も結果を届ける
  ために `notify_channel` と `notify_chat_id` が必要で、欠けていると拒否されます。
  `kind="goal"` と `schedule` の併用も拒否されます。委任ポリシーの検査（部門と階層）は、
  この入口でどの分岐よりも先に 1 回だけ行われます。
  `goals_create`（Initiative、Project、Issue 階層のノード）と `create_task`（明示的な
  `steps` を持つ複数ステップの計画を TaskSpec ディスパッチャーに渡す）は用途の異なる
  ツールで、非推奨になったことはありません。
- **スキル検索**: `skill_search` は `source` を受け取ります。`"all"`（既定。ハブと学習
  済みスキルバンクを、スキル名で重複排除）、`"github"`、`"hub"`、`"bank"` です。学習
  済みスキルバンクは今もメモリ上の空のスタブなので、`source="bank"` は空であることを
  返します。

### CLI の旧い綴り

次の綴りは今も解析されますが、新しい綴りを示す 1 行を表示し、終了コード 2 で終了します。
何も実行されません。

| 削除されたもの | 代わりに使うもの |
|---|---|
| `duduclaw migrate-from <プラットフォーム>` | `duduclaw migrate from <プラットフォーム>` |
| `duduclaw audit …` | `duduclaw export audit …` |
| `duduclaw gdpr export <連絡先>` | `duduclaw export gdpr <連絡先>` |
| `duduclaw playbook export --agent <従業員>` | `duduclaw export playbook --agent <従業員>` |
| `duduclaw acp-server` | `duduclaw acp server` |
| `duduclaw expert install <ソース>` | `duduclaw pack install <ソース>` |

`duduclaw expert install` と `duduclaw pack install` は同じインストール処理を実行する
ので、インストールされる結果は変わりません。ダッシュボードのワンクリックインストール、
アップロードインストール、AI ドラフトのインストールは、`pack install` を呼ぶようになり
ました。`duduclaw gdpr erase` と `duduclaw playbook migrate-soul` は影響を受けません。

### `config.toml [dispatch] judge`

`evaluator_only` と `human_only`（およびエイリアスの `evaluator`、`human`）は削除
されました。有効な値は `mav`（既定）と `external` だけです。ダッシュボードと
`system.update_config` は、削除された値の書き込みを拒否します。

`config.toml` に旧い値が残っている場合、ゲートウェイは次のように扱います。

| 旧い値 | ゲートウェイの現在の動作 | 対応 |
|---|---|---|
| `evaluator_only` | 検収に `mav` を使う。以前より厳しくなり、判定の費用が増える。`[dispatch] two_stage_judge`（既定で有効）は引き続き安価な evaluator を先に走らせ、完了候補のときだけパネルの費用を払う | `judge = "mav"` に変更する |
| `human_only` | 機械による検収には戻らない。検収に回ったすべての作業が `needs_human` で止まり、一時停止の理由はシステムの問題として表示され、直し方が添えられる | `judge = "mav"` または `external` に変更する。人の確認が必要な従業員には、従業員ごとの `[capabilities] autonomy_level` と `approval_required_tools` を使う。止まった作業を進めるには、タスクで「完了にする」を押すか、設定を直してから「再試行」を押す（再試行はタスクを `pending` に戻し、保存済みの結果の要約と担当の取得を消し、任意のメモを次のラウンドへの指示として使う。ラウンドの数え方は続きから始まり、すでに書き出されたファイルは削除されない） |

どちらの値も、プロセスごとに警告を 1 回記録します。ゲートウェイのプロセスごと、
データディレクトリごとに 1 回、最初の作業が検収に入ったときに、監査イベント
`judge_mode_removed` を 1 件と Activity Feed の通知を 1 件書き込みます。ゲートウェイを
再起動すると、もう一度書き込みます。監査イベント `judge_mode_deprecated` は発生しなく
なりました。`duduclaw doctor` がこの状況を一覧表示します。`human_only` は、検収に回った
すべての作業が止まるため失敗として、`evaluator_only` は警告として表示されます。
`config.toml` を読めない、または解析できないときは、検査できなかったことを示す警告に
なります。

### アップグレード前の確認

1. エージェントのプロンプト、スキル、自動化から、削除された 8 つの MCP ツール名を
   grep し、各 `agent.toml [capabilities]` のツール一覧も確認する。
2. スクリプト、cron エントリ、systemd unit から、削除された 6 つの CLI の綴りを grep
   する。削除された綴りは、実行されずに終了コード 2 で終了するようになります。
3. `config.toml [dispatch] judge` が `evaluator_only` や `human_only` のままでないか確認
   する。
4. `duduclaw doctor` を実行する。残っている削除済みツール名と、削除された judge の値
   が一覧表示されます。

---

## 引き続き非推奨

### パックの旧形式

パックの旧マニフェスト `expert.toml`、`team.toml`、業種パックのディレクトリ構成は非推奨
です。v1.69.0 でもこれらはすべて読み込まれ、削除されたものはありません。書き直された
有料テンプレートと一緒に、今後のバージョンで削除されます。バージョン番号は未定です。

**残している理由**: 新形式の `pack.toml` は、現時点では職務プリセット
（`kind = "preset"`）としてしかインストールできません。`duduclaw pack install` は
`pack.toml` のチームパックや業種パックを読み取れますが、その後ディレクトリをエキスパート
パックのインストーラーに渡します。このインストーラーは `expert.toml`（または Claude Code
プラグイン、単体の Agent Skill）だけを認識し、それ以外はすべて拒否します。インストーラー
が `pack.toml` のチームパックと業種パックをインストールできるようになるまで、旧形式は
外せません。現時点での書き方は [独自パックの作り方](build-your-own-pack.md) を参照して
ください。

| 現状 | 状態 |
|---|---|
| `expert.toml`（チームパックと業種パック） | そのまま使う。非推奨だが読み込まれる |
| `team.toml`、業種パックのディレクトリ | 非推奨。そのまま読み込まれる。ディスク上の移行は行わない |
| `pack.toml` で `kind = "preset"` | 職務プリセットの現行形式 |
| `pack.toml` で `kind = "team"` または `"template"` | 読み取りと確認（`pack inspect`）はできる。まだインストールはできない |

### Gemini CLI ランタイム

**Gemini CLI エージェントランタイム**（ランタイム id `gemini`、実行ファイル `gemini`、
npm パッケージ `@google/gemini-cli`）は **v1.67.0** で非推奨になりました。削除は
v1.69.0 から **v1.70.0** に延期されました。後継は **Antigravity CLI ランタイム**
（`antigravity`、実行ファイル `agy`）です。

| 旧 | 新 |
|---|---|
| `agent.toml [runtime] provider = "gemini"` | `provider = "antigravity"` |
| `agent.toml [runtime] fallback = "gemini"` | `fallback = "antigravity"` |
| `config.toml [runtime] utility_provider = "gemini"` | `utility_provider = "antigravity"` |
| `config.toml [dispatch] judge_provider = "gemini"` | `judge_provider = "antigravity"` |
| `[team.roles.*] runtime = "gemini"` | `runtime = "antigravity"` |
| `[discovery.attempt.runtimes.gemini]` | `[discovery.attempt.runtimes.antigravity]` |

**延期の理由**: 当初告知した削除の前提条件は、Antigravity の API キーモードを実際の
Gemini API キーで検証することでした。その検証で、既定の権限レベルの Antigravity 従業員
がプラットフォームのツールを呼び出すと、Antigravity CLI 自身に拒否されることが分かり
ました。修正は進行中です。この問題が直り、検証をやり直すまで、Gemini CLI ランタイムは
削除されません。

**まだ使えるもの**: 上記の旧い値は、これまでどおり解析・実行されます。読み込み時には
プロセスごとに警告を 1 回記録し、ダッシュボード（`agents.create` / `agents.update`）
経由で書き込むと監査イベント `runtime_provider_deprecated` も残ります。ダッシュボード
は Gemini を提示しなくなりますが、保存済みの `gemini` は「非推奨」ラベル付きで表示され
ます。エージェント編集ページのランタイム選択肢には Antigravity があります。「選択した
モデルに合わせてランタイムを揃える」処理は、非推奨のランタイムを書き込まなくなりま
した。初回セットアップウィザードの既定値は Antigravity です。Docker イメージは引き続き
Gemini CLI を同梱し、`duduclaw doctor` は `provider` または `fallback` が非推奨ランタイム
のエージェントを一覧表示します。

**非推奨ではないもの**: **Gemini API プロバイダー**（プロバイダー id `gemini`、
`GEMINI_API_KEY`、LLM 層の `generateContent` プロトコル、`gemini` プロバイダーアカウ
ント）は影響を受けません。Antigravity の API キーモードもこれを使います。

**Gemini CLI を廃止する理由**: Google は 2026-06-18 に、無料、Google AI Pro、Google AI
Ultra の個人アカウントに対する Gemini CLI での提供を停止し、Antigravity CLI への移行を
案内しています。API キーとエンタープライズ（Gemini Code Assist）のユーザーは影響を受け
ず、Gemini CLI 自体は引き続きメンテナンスされています（出典: メンテナーの
[告知](https://github.com/google-gemini/gemini-cli/discussions/28017)、Google の
[移行ガイド](https://antigravity.google/docs/cli/gcli-migration/)）。Gemini CLI が終了
したわけではありません。

**移行手順**:

1. `agent.toml` で `[runtime] provider = "gemini"`（および `fallback = "gemini"`）を
   `"antigravity"` に変更する。
2. 認証: Google アカウントでサインインする場合は、ホストのターミナルで `agy` を実行して
   ログインを完了する。Gemini CLI を API キーで認証していた場合は
   `config.toml [antigravity] auth = "api_key"` を設定し、同じ Gemini API キー
   （`gemini` プロバイダーアカウント、または `GEMINI_API_KEY`）を使い続ける。
3. モデル名: `agy models` が一覧表示する名前を使う。Gemini CLI の設定からコピーした id
   では同じモデルが選ばれないことがある。
4. `duduclaw doctor` を実行すると、`provider` または `fallback` が非推奨ランタイムの
   エージェントが一覧表示される。

v1.70.0 での削除の前に、すべての `agent.toml` で `provider = "gemini"` と
`fallback = "gemini"` を確認し、`config.toml` の `utility_provider`、
`[dispatch] judge_provider`、`[team.roles.*] runtime`、
`[discovery.attempt.runtimes.gemini]` が `gemini` のままでないかも確認してください。

---

## 非推奨リストから外した名称

次の名称は、いったん非推奨と告知されましたが、現在は通常のサポート対象の動作です。

| 名称 | 状態 | 理由 |
|---|---|---|
| `duduclaw data-migrate` | `duduclaw migrate data` の隠しエイリアスとして維持 | 出荷済みの DuDuClaw OS イメージが、読み取り専用ルートファイルシステム上の起動 unit から `duduclaw data-migrate --run` を実行するため、この綴りは使えなければならない。新しいスクリプトでは `duduclaw migrate data` を使う |
| `duduclaw migrate` | サポート。`duduclaw migrate schema` と同じ | 引数なしの形はドキュメント化された動作 |
| `duduclaw export --out …` | サポート。`duduclaw export data --out …` と同じ | 引数なしの形はドキュメント化された動作 |
| `duduclaw acp` | サポート。`duduclaw acp client` と同じ | 引数なしの形はドキュメント化された動作 |
| `duduclaw expert list` | 維持 | インストール済みの記録を一覧表示するもので、`duduclaw pack list`（インストール済みのパックとインストール可能なもの）の表示内容とは異なる |
| `preset.toml` | 維持 | 職務プリセットの保存形式。`pack.toml` に `kind = "preset"` を付ける書き方は、もう一つの作成方法 |

`preset_bindings.toml`（どの従業員にどのプリセットを適用したか）は状態でありパック形式
ではないため、非推奨になったことはありません。作成側のコマンド `expert pack`、
`publish`、`export`、`convert-teams`、`hooks`、`remove` は `duduclaw expert` の下に
残ります。
