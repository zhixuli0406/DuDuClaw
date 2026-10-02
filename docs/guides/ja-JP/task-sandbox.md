# タスクサンドボックス：委任されたタスクを施錠されたコンテナで実行する

タスクサンドボックスは、特定のエージェントに委任されたタスクを Docker コンテナの中で実行します。エージェントの AI CLI は、読み取り専用・非 root・リソース制限付きのコンテナで起動し、使い捨ての専用ワークスペースを使い、最終的な返信テキストだけが戻ります。デフォルトはオフで、エージェントごとに有効にします。

`duduclaw secaudit`（PoC ステップ）や PTC が使うスクリプトサンドボックスとは別物です。そちらは独立したコードパスで、AI CLI ではなくスクリプトを実行し、既定ではネットワークに接続しません。共通しているのはイメージだけです。スクリプトサンドボックスも同じ `config.toml [container.sandbox] image` を読み、既定は同じプラットフォームイメージで、自動ではダウンロードされません。`docker pull` を 1 回実行すれば両方で使えます。

## 前提条件

- Docker。gateway を実行するユーザーが接続できること。タスクサンドボックスは Docker のみ対応です。
- gateway を root で実行しないこと。uid 0 は何かを起動する前に拒否され、gid 0 はコンテナ作成時に拒否されます。
- 同じマシンにサンドボックスイメージがあること。デフォルトは、実行中のバージョンに対応するプラットフォーム公式イメージ `ghcr.io/zhixuli0406/duduclaw:v<バージョン>` です。自動ではダウンロードされないため、自分で実行します。

  ```bash
  docker pull ghcr.io/zhixuli0406/duduclaw:v<バージョン>
  ```

  `<バージョン>` は gateway のバージョンに置き換えるか（タグには `v` が付きます。例：`v1.67.0`）、独自のイメージを指定してください（下の設定表を参照）。
- コンテナ内の AI が使えるアカウント（[runtime ごとの認証情報](#runtime-ごとの認証情報)を参照）。
- 対応 runtime：Claude、Codex、Gemini（非推奨）、Antigravity、Grok、OpenAI 互換。

## 1 つのエージェントで有効にする

そのエージェントの `agent.toml` でサンドボックスを有効にし、ネットワークを許可します。

```toml
[container]
sandbox_enabled = true
network_access = true   # 必須：コンテナ内の AI がモデルプロバイダーに接続する必要がある
timeout_ms = 600000     # タスク 1 件あたりの制限時間（任意）
```

`network_access = false` は、コンテナを作る前に拒否されます。AI CLI はプロバイダーに接続できないと動作しないためです。サンドボックスが黙ってネットワークを開くことはありません。送信先をモデルプロバイダーだけに絞る機能は未実装です（[既知の制限](#既知の制限)を参照）。

タスクの制限時間は引き続き `agent.toml [container] timeout_ms` です。

## このスイッチの対象範囲

`sandbox_enabled = true` のとき、gateway が従業員の AI CLI を起動する経路は、次の三つのグループのどれかに入ります。

**サンドボックス内で実行されるもの。** gateway が従業員にまとまった仕事として渡すタスクです。

- 他のエージェントから委任されたタスク、またはダッシュボードから送られたタスク（エージェント間の bus）
- タスクボードから仕事を取る heartbeat の起動
- autopilot の `delegate` または `run_skill` アクション
- goal ラウンド（常に Solo。下記参照）
- 複数ステップのタスク計画の各ステップ

**サンドボックスを有効にした従業員では、その形では実行されないもの。**

- Team ラウンド：サンドボックスを有効にした従業員はチームを組みません。分解可能性ゲートは他のどのルールよりも先に、理由 `sandbox_enabled` で Solo を返します。`gate = "always_team"` よりも優先されます。そのため goal ラウンドは毎回サンドボックス内で Solo として実行され、役割メンバー（計画・実行・検証・統合）がホスト上で動くことはありません。
- Agent Mail の着信トリガーはスキップされます。トリガーされた実行の効果はサンドボックス内に存在しないプラットフォームツールに依存し、しかも信頼できない受信内容を扱うためです。メールは人が対応できるよう従業員の受信箱に残り、何も実行されません。このとき下記の監査イベントが `path = "mail"`、`action = "skipped"` で書かれます。

**設計上、引き続きホスト上で実行されるもの。** サンドボックスが意図的に渡さないプラットフォームツールと会話の状態を必要とします。

- チャネルへの返信（チャットメッセージへの応答）
- スケジュール（cron）タスク
- リマインダー
- heartbeat の能動チェック（`PROACTIVE.md`）
- 従業員の代わりに生成される ephemeral エージェント
- `duduclaw acp` セッション
- live モードの `duduclaw eval`

これらは記録なしには実行されません。サンドボックスを有効にした従業員でこれらの経路が初めて実行（またはスキップ）されると、gateway は監査イベント `task_sandbox_not_applied` を details `{path, action}` 付きで書き、ログに警告を 1 行出します。`path` は `channel_reply`、`cron`、`reminder`、`mail`、`proactive`、`ephemeral`、`acp`、`eval` のいずれか、`action` は `ran_on_host` または `skipped` です。イベントは gateway プロセスごとに（従業員、経路）の組み合わせで 1 回だけ書かれるため、忙しいチャネルが監査ログをあふれさせることはありません。gateway を再起動すると、最初の利用時にもう一度書かれます。サンドボックスが無効な従業員ではイベントは出ません。

## `config.toml [container.sandbox]` リファレンス

すべてのキーは省略可能です。サンドボックスのタスクごとに読み直されるため、変更は次のタスクから再起動なしで反映されます。未知のキーや範囲外の値があるとセクション全体が無効になり、無効なセクションはサンドボックス利用不可を意味します（デフォルト値には戻りません）。例外は `when_unavailable` と `script_when_unavailable` で、それぞれ単独で読まれるため、他のキーを直している間も回避手段は有効です。どちらも未知の値は `"fail"` として扱われます。

| キー | デフォルト | 意味 |
|---|---|---|
| `image` | `ghcr.io/zhixuli0406/duduclaw:v<実行中のバージョン>` | 実行するイメージ。ローカルに存在している必要があります。 |
| `[container.sandbox.executables]` | `claude`、`codex`、`gemini`：`/usr/bin/<名前>`、`antigravity`：`/usr/local/bin/agy`、`grok`：`/usr/local/bin/grok`、`openai-compat`：`/usr/local/bin/python3` | イメージ内の各 runtime の実行ファイルの絶対パス。キー名は `claude`、`codex`、`gemini`、`antigravity`、`grok`、`openai-compat`。カスタムイメージのときだけ必要です。 |
| `memory_bytes` | `4294967296`（4 GiB） | メモリ上限（swap はメモリと同じ）。最大 64 GiB。 |
| `pids` | `128` | プロセス数の上限。 |
| `cpu_millis` | `1000` | CPU 上限（1000 分の 1 コア単位）。 |
| `tmp_bytes` | `268435456`（256 MiB） | 書き込み可能な `/tmp`（tmpfs）のサイズ。最大 16 GiB。 |
| `workspace_bytes` | `536870912`（512 MiB） | 作業ディレクトリ `/workspace`（tmpfs）のサイズ。最大 16 GiB。 |
| `max_turns` | `30` | タスク 1 件あたりのステップ上限。 |
| `when_unavailable` | `"fail"` | タスクサンドボックスが使えないときの、委任タスクの扱い：`"fail"` または `"run_unsandboxed"`。タスクサンドボックス専用。 |
| `script_when_unavailable` | `"fail"` | スクリプトサンドボックスが使えないときの、PTC `execute_program` スクリプトの扱い：`"fail"` または `"run_unsandboxed"`。スクリプトサンドボックス専用。 |

例：

```toml
[container.sandbox]
image = "ghcr.io/zhixuli0406/duduclaw:v<バージョン>"
memory_bytes = 4294967296
max_turns = 30
when_unavailable = "fail"
```

数値はすべて正の整数である必要があります。その他の上限は、`pids` が 65536、`cpu_millis` が 256000、`max_turns` が 1000 です。2 つの tmpfs はどちらもコンテナのメモリ上限に計上されるため、`tmp_bytes + workspace_bytes` は `memory_bytes` を超えてはいけません。いずれかの規則に反するとセクション全体が無効になり、サンドボックスは利用できなくなります。

## runtime ごとの認証情報

サンドボックスは、エージェントの runtime に対応するアカウントをアカウントローテーターから取得し、そのエージェントの `account_pool` を尊重します。コンテナに渡せる認証情報だけが使えます。

| Runtime | 使えるアカウント |
|---|---|
| Claude | API キー、またはトークンを持つ OAuth アカウント（`claude setup-token` で作成）。ホストのキーチェーンにしかないログインはコンテナに入れられません。 |
| Codex | API キー、または認証ドキュメント：保存されたシークレットが CLI の `auth.json` である OAuth アカウント。 |
| Grok | API キー、または認証ドキュメント（CLI の `auth.json`）。 |
| Gemini / Antigravity | Gemini API キー。 |
| OpenAI 互換 | そのプロバイダーの API キー。 |

合うアカウントがないとタスクは失敗し、エラーにその runtime が必要とする認証情報の種類が示されます。

## サンドボックス内で使えるもの、使えないもの

使えるもの：

- 読み取り専用のルートファイルシステム、非 root ユーザー、すべての Linux capability の破棄、`no-new-privileges`。
- 上の表のメモリ・プロセス・CPU 制限。
- 作業ディレクトリ `/workspace`：サイズ `workspace_bytes` の tmpfs で、所有者はコンテナの実行ユーザー、コンテナとともに破棄されます。タスクがここに書いたものはホストのディスクに残りません。
- 書き込み可能な `/tmp`（サイズ `tmp_bytes` の tmpfs）。
- エージェントのディレクトリの一部の項目。存在する場合のみ、それぞれ個別に `/agent/<名前>` へ読み取り専用でマウントされます：`SOUL.md`、`IDENTITY.md`、`CLAUDE.md`、`AGENTS.md`、`GEMINI.md`、`CONTRACT.toml`、`SKILLS/`、`wiki/`。シンボリックリンク、ハードリンクされたファイル、種類が違うもの（ディレクトリのはずの場所にファイルがある、またはその逆）、解決するとエージェントのディレクトリの外を指すものはスキップされ、gateway のログに警告が出ます。
- ファイルツールとシェルツール。

使えないもの：

- エージェントのディレクトリのその他の部分。`.mcp.json`（エージェントの MCP キーと ID トークンを含む）、`.claude/`、`state/`、`agent.toml`、データベース、上に挙げていないすべての項目はマウントされません。
- プラットフォームの MCP ツール（メモリ、タスク、チャネル）。
- Web ツールとサブエージェント。
- エージェントのディレクトリへの書き戻し。サンドボックス内のファイル変更は破棄され、タスクの出力は最終的な返信テキストだけです。

AI が許可範囲（ファイルとシェル）外のツールを使うと、タスクは停止され、監査イベント `task_sandbox_tool_violation` が書かれます。ステップ上限に達した場合は最後の返信テキストが返り、返信がなければエラーになります。

## サンドボックスが使えないとき

`sandbox_enabled = true` の場合、次のいずれかに当てはまるとタスクは失敗します（隔離なしで実行されることはありません）。Docker に接続できない、イメージがない、gateway が root で動いている、ホストが unix 系でない、設定が無効、`network_access = false`、使えるアカウントがない、runtime が未対応。失敗のたびに理由コード付きの監査イベント `task_sandbox_unavailable` が書かれ、読みやすいエラーが返ります。

回避手段：`config.toml [container.sandbox]` に `when_unavailable = "run_unsandboxed"` を設定します。すると以前のバージョンと同じようにホスト上で隔離なしに実行され、そのたびに監査イベント `task_sandbox_bypassed` が書かれます。隔離なしの実行を受け入れる場合にだけ使ってください。

PTC の `execute_program` が使うスクリプトサンドボックスには専用のキー `script_when_unavailable` があります。`when_unavailable` はこれに影響せず、2 つのキーは互いに影響しません。分けているのはリスクが異なるためです。一方は委任された AI タスクを隔離なしでホスト上で動かすこと、もう一方は投入されたスクリプトをホスト上で動かすことを許します。既定の `"fail"` では、スクリプトサンドボックスが使えないときスクリプトは一切実行されません。ツールは `docker pull <image>` のコマンドを含む `Script sandbox unavailable (<コード>): …` を返し、監査イベント `script_sandbox_unavailable` が書かれます。`script_when_unavailable = "run_unsandboxed"` では、以前のバージョンと同じくスクリプトがホスト上で実行され、そのたびに `script_sandbox_bypassed` が書かれます。どちらのイベントにも `reason` とスクリプトの `language` が含まれます。理由コードは `invalid_config`、`no_runtime`、`runtime_unhealthy`、`image_missing`、`create_failed`、`start_failed` です。`duduclaw secaudit` の PoC ステップには回避手段がなく、ホスト上で実行されることはありません。

## 残骸の掃除

タスクのコンテナは `--rm` と `--pull never` 付きで作成され、タスクが終わると gateway が削除します。タスクの途中で gateway が強制終了された場合は、バックグラウンドの掃除が残りを削除します。掃除は gateway 起動時に実行され、その後は 10 分ごとに実行されます。対象はこの gateway home のものだけで、同じ Docker デーモン上の他の home のコンテナには触れません。

- 終了した（exited）または dead のコンテナ、および期限を 600 秒より長く過ぎたコンテナ
- 残っているどのコンテナも使っておらず、600 秒より古い `<home>/sandbox/runs/` 配下の実行ディレクトリ

Docker のコンテナ一覧を取得できない場合は何も削除しません。バックグラウンドの掃除でもタスク終了時の削除でも、失敗すると `reason` と `count` を含む監査イベント `task_sandbox_cleanup_failed` が書かれます。サンドボックスを一度も使っていない home は、Docker に接続せずにスキップされます。

## `duduclaw doctor` で確認する

```bash
duduclaw doctor
```

タスクサンドボックスの項目は、Docker に接続できるか、サンドボックスイメージがローカルにあるか、どのエージェントがサンドボックスを有効にしているかを報告し、`network_access = false` のエージェントには警告を出します。サンドボックスを有効にした従業員が 1 人以上いる場合は、その従業員のチャネル返信・スケジュールタスク・リマインダーは引き続きホスト上で実行されること、goal ラウンドは常に Solo（チームを組まない）であること、新着メールでは起動されないことを示す行が追加されます。前提条件を確認するだけで、タスクは実行しません。

## トラブルシューティング

サンドボックスのタスクが失敗すると、そのタスクを委任した側に返信が届きます。サンドボックスが実行を拒否した場合の返信は `⚠️ 子任務未執行（任務沙箱）：Task sandbox unavailable (<コード>): …` で、下の表の最初のグループに対応します。タスクが開始してから失敗した場合は `⚠️ 子任務失敗（任務沙箱，<コード>）：<メッセージ>` です。返信に含まれるのは原因と対処だけです。AI CLI や Docker の生の出力は返信には入らず、gateway のログに書き込まれます（失敗ごとに `warn` 1 行、エージェントと原因コード付き）。

| 表示 | 原因 | 対処 |
|---|---|---|
| `(docker_unreachable)` | Docker デーモンが動いていない、または gateway ユーザーが使えない。 | Docker を起動し、gateway ユーザーが `docker version` を実行できるか確認する。 |
| `(image_missing)` | イメージがこのマシンにない。 | エラーに示された `docker pull <image>` を実行するか、`image` を既にあるイメージに向ける。 |
| `(network_disabled)` | エージェントが `network_access = false`。 | そのエージェントの `[container]` で `network_access = true` にする。 |
| `(root_user)` | gateway が uid 0 で動いている。 | 通常のユーザーで gateway を実行する。 |
| `(unsupported_platform)` | ホストが unix 系ではない（Windows など）。 | Linux または macOS で gateway を実行する。 |
| `(no_account)` | コンテナ内でこの runtime を動かせるアカウントがない。 | エラーに示された種類のアカウントを追加する（認証情報の表を参照）。 |
| `(unsupported_runtime)` | そのエージェントの runtime はサンドボックスで実行できない。 | 対応 runtime を使う。 |
| `(invalid_config)` | `[container.sandbox]` に不正な値がある（`tmp_bytes + workspace_bytes` が `memory_bytes` を超える場合を含む）、またはエージェントにモデル未設定・タイムアウト 0。 | エラーが示すキーを修正する。 |
| `authentication failed: ...` | プロバイダーが認証情報を拒否した。 | エラーに示されたアカウントの認証情報を差し替える。 |
| `... rate-limited or rejected the request for quota` | プロバイダーのクォータ。 | 待つか、アカウントを追加する。 |
| `... used a tool outside the allowed file/shell surface` | AI がサンドボックスで許可されていないツールを使おうとした。 | サンドボックスなしのエージェントに任せるか、タスクを変更する。 |
| `... reached the sandbox step limit` | 返信前に `max_turns` に達した。 | `max_turns` を上げるか、タスクを分割する。 |
| `... timed out after N s` | エージェントの `timeout_ms` に達した。 | `timeout_ms` を上げる。 |
| `... sandbox container refused: the gateway runs as root (uid or gid 0)` | gateway のグループ ID が 0。 | プライマリグループが root でないユーザーで gateway を実行する。 |
| `Script sandbox unavailable (<コード>): ...`（`execute_program` から） | スクリプトサンドボックスが使えず、`script_when_unavailable` が `"fail"`。コードは `invalid_config`、`no_runtime`、`runtime_unhealthy`、`image_missing`、`create_failed`、`start_failed` のいずれか。 | Docker を起動し、エラーに示された `docker pull <image>` を実行する。`invalid_config` なら `[container.sandbox]` を修正する。スクリプトをホストで実行してよい場合に限り `script_when_unavailable = "run_unsandboxed"` を設定する。 |

## 既知の制限

- 送信先はモデルプロバイダーに限定されません。コンテナは通常の bridge ネットワークを使います。そのため、ホストのネットワークから届く場所にはすべて届きます。クラウドのメタデータエンドポイント（`169.254.169.254`）や、Docker Desktop ではホスト上で待ち受けているサービスも含まれます。
- AI CLI に渡した認証情報は、コンテナ内の AI から見えます（環境変数と CLI 自身の認証ファイル）。送信先が制限されていないため、外部へ送ることもできます。漏えいしても許容でき、すぐにローテーションできるアカウントを使ってください。メインのアカウントは使わないでください。
- サンドボックスに入るのは[このスイッチの対象範囲](#このスイッチの対象範囲)の最初のグループに挙げたタスクだけです。チャネル返信、cron タスク、リマインダー、同じ節に挙げたその他のホスト経路は隔離されません。監査イベント `task_sandbox_not_applied` は実行されたことを記録するだけで、止めはしません。
- サンドボックス内にプラットフォームの MCP ツールはありません。
- ファイルの出力はエージェントのディレクトリに書き戻されません。
- Docker のみ。タスクサンドボックスは Apple Container も WSL2 も使いません。
- イメージは自動ダウンロードされません。
- スクリプトサンドボックスには起動時の掃除がありません。スクリプトを実行しているプロセス（エージェントに対応する `duduclaw mcp-server`、または `duduclaw secaudit`）が途中で終了すると、スクリプトのコンテナはスクリプトが自分で終了するまで動き続けます。
- Windows では、スクリプトサンドボックスはまず WSL2、次に Docker を試します。WSL2 の経路（Windows パスから WSL パスへの変換を含む）はクロスコンパイルしただけで、実際の Windows ホストではまだ実行されていません。
- gateway 自体がコンテナ内で動いている場合のタスクサンドボックスは、このガイドの対象外で、検証もされていません。
- 実際に動かした範囲：公開済みのプラットフォームイメージ（`v1.66.1`）で、Codex、Antigravity、Grok の CLI がサンドボックス内で起動し、それぞれのプロバイダーに到達することを確認しました（無効なキーを使い、プロバイダーが拒否することで確認）。また、Codex で委任タスクを 1 回最後まで実行しました（このときは公開イメージではなく、同じ CLI を入れてローカルでビルドしたイメージを使いました）。Claude、Gemini CLI、OpenAI 互換 runtime は、サンドボックス内で実際のプロバイダーに対してはまだ実行していません。
