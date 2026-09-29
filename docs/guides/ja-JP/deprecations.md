# 非推奨となった名称

このページに載っている名称はまだ使えますが、いずれ削除されます。現時点で削除された
ものは一つもありません。古い名称はこれまでどおり受け付けられ、動作も変わりません。
MCP ツールは `tools/list` にも残っているので、古い名前を覚えたモデルはそのまま
呼び出せます。

**方針**: 古い名称は **マイナーバージョン 2 つ分** 残します。このページの項目はすべて
**v1.66.0** で非推奨になり、**v1.68.0** での削除を予定しています。

各面での示し方:

| 面 | 示し方 | まだ使える？ |
|---|---|---|
| MCP ツール | `description` の先頭に `[deprecated → <新ツール> <パラメータ>]`、`tool_catalog` で `deprecated: true` | 使える。`tools/list` に残り、呼び出しも可能。隠すと「呼び出せなく」なり、非推奨期間の目的と正反対になる |
| CLI サブコマンド | clap の `hide = true` — `--help` には出ないが解析される | 使える |
| `config.toml` の値 | 読み込み時にプロセスごと 1 回 `warn!`、ダッシュボード経由の書き込み時には監査イベント | 使える。設定された挙動が黙って置き換わることはない |
| ダッシュボード | 新しい名前だけを提示。保存済みの非推奨値は「非推奨」ラベル付きで表示 | 使える |

---

## MCP ツール

### wiki: `wiki_*` 一式に `scope` パラメータ

`wiki_*` と `shared_wiki_*` は、どちらの wiki を指すかだけが違うほぼ鏡像の API でした。
これを `scope: "agent" | "shared"` を持つ一式に統合しました。既定は `agent` なので、
既存の `wiki_*` 呼び出しは一切変わりません。

| 旧 | 新 |
|---|---|
| `shared_wiki_ls` | `wiki_ls` に `scope="shared"` |
| `shared_wiki_read` | `wiki_read` に `scope="shared"` |
| `shared_wiki_write` | `wiki_write` に `scope="shared"` |
| `shared_wiki_search` | `wiki_search` に `scope="shared"` |
| `shared_wiki_stats` | `wiki_stats` に `scope="shared"` |
| `shared_wiki_lint` | `wiki_lint` に `scope="shared"` |

変わらないもの: `.scope.toml` の名前空間 SoT ポリシー、`wiki_visible_to` の可視性、
削除時の「原著者またはメインエージェントのみ」判定。動いたのは入口だけです。

**`shared_wiki_delete` は意図的に統合していません**。エージェントローカル側に対応
するものが無いため、`wiki_delete` に `scope="agent"` を作ると、重複を「減らす」のでは
なく破壊的な権限を「増やす」ことになります。名前はそのまま、非推奨にもしません。

### 作業の作成: `tasks_create` に一本化

| 旧 | 新 |
|---|---|
| `schedule_task`（繰り返しの cron） | `tasks_create` に `schedule="<cron 式>"` |

`tasks_create` に任意パラメータが 2 つ増えました。

- **`kind`** — `"task"`（既定、カンバンのタスク）か `"goal"`（自律ゴール: 受け入れ
  基準を作成時に凍結し、完了かどうかは AI 判定パネルが決める。任意の `plan_first`
  で計画を作って人の承認待ちにできる）。`kind="goal"` はダッシュボードの依頼シートと
  同じコード経路を通ります。
- **`schedule`** — cron 式（5 または 6 フィールド）なら繰り返し作業、RFC3339 の時刻
  （`2026-10-01T09:00:00+08:00`）なら 1 回だけの起動。どちらも結果を届けるために
  `notify_channel` と `notify_chat_id` が必要で、1 回きりの指定でそれが無い場合は
  届かないリマインダーを作らずに拒否します。

`kind="goal"` と `schedule` は併用できません。ゴールは一度だけ完了まで走るものです。
この組み合わせは丸ごと拒否され、中途半端に適用されることはありません。

**`goals_create` と `create_task` はいずれも非推奨ではありません**。`goals_create`
はゴール「階層」のノード（Initiative → Project → Issue）— 担当者が見る why-chain —
を作ります。`create_task` は明示的な `steps` 配列からなる複数ステップの計画を
TaskSpec ディスパッチャに渡すもので、`tasks_create` には対応するパラメータが
ありません。非推奨にすると、存在しない代替を約束することになります。両方の説明文
に、自分が何であるか、そして自分の担当ではないケースでは `tasks_create` を使うこと
を明記しました。

委任ポリシー（部署 × 階層）の判定は、**統合された入口で一度だけ**、どの分岐よりも
先に行われます。これが統合のセキュリティ上の要点です。呼び出し側が、古い 4 つの
ツールのうち最も検査が緩いものを選んで部署をまたぐ割り当てを通す、ということが
できなくなります。

### スキル検索: `skill_search` に `source` パラメータ

| 旧 | 新 |
|---|---|
| `skill_bank_search` | `skill_search` に `source="bank"` |

`skill_search` に `source` が増えました。

- `"all"`（既定）— 設定済みのスキルハブ **と** この環境が学習したスキルバンクを
  検索し、スキル名で重複排除する
- `"github"` — GitHub ハブのみ
- `"hub"` — キュレーション済みレジストリ
- `"bank"` — 学習済みスキルバンクのみ

モデル向けの一文ルール: そのスキルがどこにあるか分かっている場合を除き、`source` は
触らないこと。

学習済みスキルバンクはまだ空のインメモリスタブなので、`source="bank"` は正直に
「空である」と返します。ハブの結果でごまかすことはありません。

---

## CLI サブコマンド

古い綴りは `--help` から隠れますが、解析は従来どおりです。

### `migrate`

互いに無関係な 3 つのコマンドで、ヘルプ文で互いを打ち消す必要がありました。

| 旧 | 新 |
|---|---|
| `duduclaw migrate` | `duduclaw migrate schema`（素の `duduclaw migrate` も引き続きこの意味） |
| `duduclaw migrate-from <platform>` | `duduclaw migrate from <platform>` |
| `duduclaw data-migrate` | `duduclaw migrate data` |

### `export`

意味がまったく違う 4 つのエクスポートが、所属グループだけで区別されていました。

| 旧 | 新 |
|---|---|
| `duduclaw export --out …` | `duduclaw export data --out …`（素の形も引き続きこの意味） |
| `duduclaw audit …` | `duduclaw export audit …` |
| `duduclaw gdpr export <contact>` | `duduclaw export gdpr <contact>` |
| `duduclaw playbook export --agent …` | `duduclaw export playbook --agent …` |

`duduclaw gdpr erase` と `duduclaw playbook migrate-soul` は影響を受けません。

### `acp`

doc コメントの注意書きだけで区別されていた 2 つの別プロトコル:

| 旧 | 新 |
|---|---|
| `duduclaw acp`（エディタ向け Agent Client Protocol） | `duduclaw acp client`（素の `duduclaw acp` も引き続きこの意味） |
| `duduclaw acp-server`（A2A エージェント間） | `duduclaw acp server` |

---

### `pack`

3 つのインストール動詞が指していたのは、いつも同じもの――あらかじめ構成された AI 従業員の一式です。`duduclaw pack` が唯一の入口になりました（T5/O2）。`duduclaw expert install`／`expert list` は同じコードのエイリアスで、旧マニフェスト方言はすべてそのまま読み込まれます（ディスク移行なし）。

| 旧 | 新 |
|---|---|
| `duduclaw expert install <src>` | `duduclaw pack install <src>` |
| `duduclaw expert list` | `duduclaw pack list` |
| `expert.toml`（エキスパートパックのマニフェスト） | `pack.toml`（`kind = "team"`） |
| `team.toml`（有料チームプレイブック） | `pack.toml`（`kind = "team"`、`tier = "premium"`） |
| `preset.toml`（職務プリセットの内容ファイル） | `pack.toml`（`kind = "preset"`） |

`preset_bindings.toml`（どの従業員にどのプリセットを適用したか）は状態でありパック形式ではないため、非推奨の対象外です。作成側の動詞（`expert pack`／`publish`／`export`／`convert-teams`／`hooks`／`remove`）は `duduclaw expert` の下に残ります。

## 設定値

### `[dispatch] judge`

| 旧い値 | 移行先 | 理由 |
|---|---|---|
| `evaluator_only` | `mav` | `[dispatch] two_stage_judge`（既定で有効）が先に安価な evaluator を走らせ、完了候補になったときだけパネルの費用を払うため、コスト面の動機はすでに満たされている |
| `human_only` | `mav` ＋ エージェント単位の `[capabilities] autonomy_level` / `approval_required_tools` | 人が見るべき所で人を待たせる。プラットフォーム全体で機械判定を止める必要はない |

`mav` と `external` は影響を受けません。4 つの値はすべて引き続き解析されます。
非推奨モードで運用中の環境は設定どおりに動作し、プロセスごとに警告を 1 回記録
します。ダッシュボード経由で書き込まれた場合は `judge_mode_deprecated` の監査
イベントも残ります。ダッシュボードは `mav` と `external` だけを提示しますが、保存済み
の非推奨値はラベル付きで表示し、黙って切り替えることはありません。

---

## v1.68.0 で起きること

上記の古い名称はすべて削除されます。v1.67.x より先へ上げる前に:

1. エージェントのプロンプト、スキル、自動化から古い MCP ツール名を grep する。
2. スクリプト、cron エントリ、systemd unit から古い CLI の綴りを grep する。
3. `config.toml [dispatch] judge` が非推奨の値のままでないか確認する。

対象ツールの説明の先頭にある `[deprecated → …]` は、grep できるように付けています。
