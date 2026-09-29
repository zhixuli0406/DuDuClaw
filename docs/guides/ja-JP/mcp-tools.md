# MCP ツール: エージェントに何が見え、なぜそうなるのか

DuDuClaw の MCP サーバーは標準の `tools/list` でツールを宣言します。このページでは、誤解されやすい 2 点を説明します。

1. なぜエージェントに見えるツールが、サーバーが実装しているものより少ないのか。
2. ツール説明に字数上限が入った今、長い詳細はどこへ行ったのか。

ツールを追加する場合は先に [custom-mcp-tool.md](../custom-mcp-tool.md) を読んでください。このページは実装ではなく宣言面の話です。

## ルール: 見えること ⇔ 呼べること

`tools/list` は、呼び出し元が**その時点で実際に呼べる**ツールだけを列挙します。以下のフィルタはいずれもディスパッチャが既に強制しているゲートを写したものなので、エージェントが読むリストとサーバーが受け付ける呼び出しは常に同じ集合を指します。

これは正しさの問題であると同時にコストの問題でもあります。ツールスキーマは**起動のたびに支払う固定のプロンプトコスト**です。CLI はセッションごとに `tools/list` を一度読み、その全体が最初のユーザートークンより前にモデルのコンテキストに入ります。ゲートが拒否するツールのスキーマは二重の損です。トークンを払い、さらにモデルが使えないツールを前提に計画を立てます。

ツールを隠すことは**認可の判断ではありません**。未掲載のツールを呼んでも本物のゲートに到達し、ゲート自身のメッセージで拒否されます。

### フィルタの一覧

| フィルタ | 参照元 | オフ / 空のときの効果 |
|---|---|---|
| 外部クライアント許可リスト | `principal.is_external` | 7 ツールのみ掲載 |
| Google Workspace | `config.toml [integrations] google_workspace` | Google 系 19 ツールを非表示 |
| GitHub | `config.toml [integrations] github` | `github_*` 5 ツールを非表示 |
| `denied_tools` / `allowed_tools` | `agent.toml [capabilities]` | 拒否分を非表示。許可リストが非空なら他を全て非表示 |
| `os_native` | `agent.toml [capabilities]` | `os_*` 自動化 6 ツールを非表示 |
| `recording` | `agent.toml [capabilities]` | 録画系 5 ツールを非表示 |
| `system_operator` | `agent.toml [capabilities]` | アプライアンス操作 19 ツールを非表示 |
| `codrive` | `agent.toml [capabilities]` | `codrive_run` / `codrive_status` を非表示 |
| `computer_use` | `agent.toml [capabilities]` | `computer_*` 7 ツールを非表示 |
| `db_sources` | `agent.toml [capabilities]` | `db_*` 4 ツールを非表示 |
| `[fork] enabled` | `agent.toml` | 分岐系 6 ツールを非表示 |
| `scoped_tools` | `agent.toml [capabilities]` ＋ 有効な付与 | タスクスコープの付与が有効になるまで非表示 |

新規作成したエージェントは上記のいずれも有効ではありません。それが狙いで、既定の構成は使えるツールの分だけを支払います。

### セッション途中の変更がどう届くか

かつてツールを隠すことは恒久的に到達不能にすることを意味しました。MCP クライアントはセッション開始時に一度だけ `tools/list` を読むからです。現在サーバーは `initialize` 応答で `tools.listChanged` を宣言し、呼び出し元に見える集合が**実際に変わったとき**に `notifications/tools/list_changed` を送ります。比較は集合単位なので、設定ファイルに触れただけで内容が同じなら何も送られません。

したがって次の流れは再起動なしで動きます。

1. エージェントが `scoped_tools` の拒否に当たる、または運用者が権限を付与したい。
2. `capability_request` が承認される（またはダッシュボードが `agent_update` を保存、あるいは誰かが `agent.toml` を編集）。
3. 数秒でサーバーが変更を検知しクライアントへ通知。
4. クライアントが `tools/list` を読み直し、ツールが現れる。

タスクスコープの付与はタスクが終端状態に入るたび失効し、同じ仕組みでツールも再び見えなくなります。

### 注意: エージェントは `scoped_tools` の名前を自力で発見できません

`scoped_tools` に載ったツールは付与が有効になるまで非表示なので、エージェントが `tools/list` から名前を読んで申請することはできません。別の手段で名前を伝えてください——`SOUL.md` に書く、playbook ルールにする、あるいはゴール開始時に `grant:<tool>` タグで付与を発行する。剪定が代償を伴う唯一の箇所であり、意図的なものです。今この瞬間に拒否されるツールを宣言することこそ、上のルールが取り除こうとしている失敗モードだからです。

## 説明のバイト予算

各ツールの `description` は **200 バイト**、各パラメータの説明も **200 バイト**が上限です（明記された例外が 1 件だけあります）。この上限は慣習ではなくテストで強制されます。

ツールを追加・編集する人にとっての帰結は 2 つです。

- **何をするか、何が拒否されるかを書く。** 安全上重要な一文——「これは送信しない」「これは公開される」「超過分は丸ごと拒否、決して切り詰めない」——は説明に残します。設計理由・実例・内部ラベルは残しません。
- **長い版はこのページに書く。** 説明からこのページの節、あるいはそのツール自身の仕様ページへリンクします。

### 唯一の例外

`team_handoff` の `packet` パラメータは TaskPacket の完全な形を保持します（上限 1,024 バイト）。`build_tool_schema` は全パラメータを素の JSON-Schema 文字列として宣言するため、実際の形を書ける場所はパラメータ説明だけです。さらに TaskPacket は切り詰めずに丸ごと拒否されるので、形が見えないエージェントは有効なパケットを作れません。完全な仕様は [../../spec/task-packet.md](../../spec/task-packet.md) です。

例外は 1 つのリスト（`PARAM_CAP_EXEMPTIONS`）にまとめられ、例外が不要になったときにテストが落ちます。

## 説明から移した長い詳細

### `codrive_run` — 3 段の実行ラダー

各ステップは次の順に 3 段を試し、当てはまる最上段を優先します。

- **C-L2 — `api_action`。** GUI に触れる前に、`target_app` の登録済みサードパーティネイティブ API/CLI/D-Bus アクションを呼びます。必要な app とアクションが登録済みなら常にこちらを優先します。`action` は短い登録識別子（chromium の `open_url`、networkmanager の `state` など）、`params` はそのアクションのペイロードで、ディスパッチ時にアクション自身のスキーマで検証されます。登録なしや実行失敗はステップの `action` にフォールバックするため、`api_action` を設定していても `action` は必須です。
- **C-L3 — `locate`。** `move`/`click` の座標を、手書きのピクセルではなく `target_app` の AT-SPI2 アクセシビリティツリー上の `(role, name)` 参照で解決します。レイアウト・解像度・テーマの変更にはるかに強い方法です。`text` / `key_name` / `wait` / `take_over` では無視されます。見つからない場合は素の `x`/`y` にフォールバックします。
- **C-L1 — 素の `x`/`y`。** 上の 2 段が無いか失敗したときの最終手段。

その他のスクリプトレベルの規則:

- `target_app` は**スクリプト単位の単一フィールド**で、ステップ単位ではありません。1 スクリプトは 1 app を操作します。別の app を操作するには `codrive_run` をもう一度呼びます。
- 結果を伴うステップ（`send` / `submit` / `delete` / `purchase` / `other`）は、どの段がディスパッチする前にも人間の承認で停止します。拒否リスト該当（銀行ページ、CAPTCHA 回避など）は接続を試みる前に即座に拒否されます。
- ログイン／パスワード／支払いのステップ（`take_over`、または `credential` クラス）は共有デスクトップを人間に渡します。資格情報のテキストをどの段からも自分で送ることはありません。人が操作を返した時点でスクリプトが再開します。
- 共有デスクトップで人間の入力があれば、エージェントの席は即座に凍結します。落ちたステップは人が操作を返した後に一度だけ再試行されます。
- `watch_mode: true` で、実行の残り時間にわたりアイドル監視が有効になります。
- 最大 50 ステップ。

### `working_state_handoff` — 構造化モード

2 つのモードがあります。

- **素のノート。** `status` を省略すると従来どおりの挙動で、約 1,200 文字で黙って切り詰められます。
- **構造化（Ralph ループ方式）。** `status` を渡すと `next_steps` / `evidence` / `blocker` と併せて検証されます。
  - `continue` — `next_steps` が必須、`blocker` は不可。
  - `complete` — `evidence` が必須、`blocker` と `next_steps` はいずれも不可。証拠のない自己申告の完了や、次の手順が残ったままの完了は拒否されます。「終わりました」は証拠ではありません。
  - `blocked` — 具体的な `blocker` が必須。

  結合後のペイロードは `config.toml [memory] working_state_handoff_max_bytes`（既定 16384、CJK 安全なバイト数）で制限されます。超過は**呼び出し全体を拒否**し、決して黙って切り詰めません。切り詰めれば、この引き継ぎを権威たらしめている証拠そのものを消しかねないからです。

### `skill_search` — source の選び方

そのスキルがどこにあるか既に分かっている場合を除き、`source` は触らないでください。

- `all`（既定）— 設定済みハブとこのエージェントが学習したスキルバンクを検索し、名前で重複排除して出典を付けます。ハブ結果は関連度 × 信頼 × インストール数 × 鮮度で順位付けされ、公式ファーストパーティのスキルが上位に底上げされます。
- `github` — 公開 GitHub リポジトリにあると分かっているスキル。
- `hub` — キュレーション済みレジストリ（`anthropic-skills`、`github`、`clawhub`、`lobehub`、`skills-sh`）。`hub` パラメータで 1 つに絞れます。
- `bank` — この構成が自力で学習したものだけ。この source では `hub` パラメータは受け付けません。

### `evolution_toggle` — 停滞検知のサブフィールド

標準フラグに加えて `field` は `stagnation_enabled`（bool）、`stagnation_window_seconds`（60–604800）、`stagnation_trigger_threshold`（1–1000）、`stagnation_action`（`log_only` | `suppress`）を受け付けます。[evolution-switches.md](../evolution-switches.md) を参照してください。

## 非推奨エイリアスは引き続き掲載されます

非推奨のツール名は説明に `[deprecated → …]` の接頭辞を付けたまま `tools/list` に残ります。隠せば呼べなくなり、それは非推奨期間の目的と正反対だからです。旧 → 新の完全な対照表は [deprecations.md](../deprecations.md) にあります。

## 関連文書

- [custom-mcp-tool.md](../custom-mcp-tool.md) — ツールの追加
- [mcp-bridge.md](../mcp-bridge.md) — 外部 MCP サーバーのマウント
- [remote-mcp.md](../remote-mcp.md) — HTTP/OAuth トランスポート
- [../../spec/task-packet.md](../../spec/task-packet.md) — TaskPacket 仕様
- [../../spec/reversible-context-ccr.md](../../spec/reversible-context-ccr.md) — `duduclaw_ccr_*` ツール。MCP サーバーではなく直接 API のツールループが注入します
