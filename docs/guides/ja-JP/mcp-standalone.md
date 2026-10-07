# MCP サーバーを単体で使う

DuDuClaw の MCP サーバーは DuDuClaw gateway なしで単体実行でき、Claude Code・Codex・Cursor にセッションをまたいで残るメモリと Markdown wiki を提供します。このページでは、使えるツール、設定手順、フルプラットフォームとの関係を説明します。

## 設定

Node.js（`npx` のため）が必要です。グローバルには何もインストールしません。

### Claude Code

```bash
npx duduclaw mcp init --client claude-code
```

`~/.duduclaw` がなければ作成し、キーを発行し、確認のうえで `claude mcp add` を実行します（user スコープ）。`--yes` を付けると確認なしで実行します。新しい Claude Code セッションで `/mcp` を実行し、`duduclaw` が接続済みになっていることを確認してください。

Claude Code にすでに `duduclaw` がある場合は、先にその中身を確認します。以前の `mcp init` が書いたエントリはそのまま置き換えます。それ以外（別のキー、同名の別サーバー、読めない設定ファイル）は、キーを発行する前に停止し、そのエントリを秘密値を伏せて表示します。置き換えるには `--replace` を付けてください。置き換える前に古いエントリを `~/.duduclaw/mcp_init/claude-duduclaw-<時刻>.json`（本人のみ読める）に保存し、新しいエントリを追加できなかった場合は古いものを戻します。

Claude Code CLI がない場合や確認で「いいえ」と答えた場合は、実行すべき `claude mcp add …` の行をそのまま表示します。

### Codex

```bash
npx duduclaw mcp init --client codex
```

`~/.codex/config.toml` に貼り付けるブロックを表示します。

```toml
[mcp_servers.duduclaw]
command = "npx"
args = ["-y", "duduclaw@<バージョン>", "mcp-server"]
env = { DUDUCLAW_MCP_API_KEY = "ddc_refresh_prod_…" }
```

`<バージョン>` は `mcp init` を実行したバージョン（`duduclaw --version` の表示）です。設定はこのバージョンに固定されるので、クライアントが起動するサーバーはキーを発行したものと同じです。アップグレード後は `mcp init` を再実行してください。

### Cursor

```bash
npx duduclaw mcp init --client cursor
```

`~/.cursor/mcp.json` のエントリを表示します。ファイルに他のサーバーがある場合は、`duduclaw` のオブジェクトだけを `mcpServers` にコピーしてください。

### コマンドの補足

- `--client` を省略すると 3 種類の設定をすべて表示し、どのクライアントにも登録しません。ただし毎回と同じく新しいキーは発行されます（有効期間 90 日）。
- `--client` の値ごとにキーが別になり、メモリの名前空間と wiki も分かれます。`--client print` のキーは `external/standalone-claude-code` ではなく `external/standalone-print` に書き込みます。実際に接続するクライアントの値を使ってください。
- `duduclaw` をグローバルにインストール済み（`npm install -g duduclaw`）なら、設定は `npx` ではなくインストール済みのバイナリを指します。パスはバイナリが起動されたときのもので、リンクは展開しません。パスが特定の Node バージョン（nvm、volta）に属する場合は、バージョンを切り替えたあと `mcp init` を再実行してください。
- 表示される `claude mcp add` の行は bash／zsh 向けの単一引用符です。Windows 版では PowerShell／cmd 向けの二重引用符になります。
- `DUDUCLAW_HOME` を設定して実行した場合、表示される設定にも同じ `DUDUCLAW_HOME` が入り、サーバーは同じデータディレクトリを開きます。
- キーは一度だけ表示され、DuDuClaw は `~/.duduclaw/mcp_tokens.db` にハッシュだけを保存します。クライアントはキーを自分の設定ファイル（`~/.claude.json`、`~/.codex/config.toml`、`~/.cursor/mcp.json`）に平文で保存します。そのファイルを読める人は、キーが期限切れになるか失効するまで使えます。有効期間は 90 日です。新しいキーは `mcp init` を再実行して取得し、動作を確認してから `duduclaw mcp revoke-token <jti>` で古いキーを失効させてください（同じクライアントの有効な古いキーはコマンドが一覧表示します）。`duduclaw mcp list-tokens` で全キーを確認できます。
- DuDuClaw の AI 社員のセッション内では実行を拒否し、原因の環境変数名を表示します。自分のシェルで `DUDUCLAW_MCP_API_KEY` を export している場合も該当するので、unset してから再実行してください。

## 使えるツール

キーのスコープは `memory:read`、`memory:write`、`wiki:read`、`wiki:write` の 4 つです。`tools/list` には、これらのスコープで呼べる 24 ツールがちょうど表示されます。

| 分類 | ツール |
|---|---|
| メモリ | `memory_store`、`memory_search`、`memory_read`、`memory_fetch_batch`、`memory_get_history`、`memory_get_at`、`memory_alias_add`、`memory_alias_list`、`memory_improve` |
| ユーザープロファイル | `user_profile_record`、`user_profile_get`、`user_code_profile` |
| コードマップ | `code_map`（`root` で指定したディレクトリ、省略時はサーバーの作業ディレクトリ） |
| Wiki | `wiki_write`、`wiki_read`、`wiki_ls`、`wiki_search`、`wiki_stats`、`wiki_lint`、`wiki_graph`、`wiki_export`、`wiki_dedup`、`wiki_rebuild_fts`、`wiki_share` |

データの保存先：

- メモリ：`~/.duduclaw/memory.db`、名前空間 `external/standalone-<クライアント>`（例：`external/standalone-claude-code`）。`mcp init` を個別に実行したクライアントごとに名前空間が分かれます。
- Wiki：`~/.duduclaw/agents/standalone-<クライアント>/wiki/`。
- 共有 wiki（`~/.duduclaw/shared/wiki/`）：`wiki_share` は自分のページの要約を `sources/standalone-<クライアント>--<ページ名>.md` としてコピーし、作者はクライアント id になります。`scope="shared"` 付きの `wiki_write` は拒否されます（`-32003`）。共有 wiki への書き込みは AI 社員として行われるもので、このキーは社員ではないためです。共有 wiki の読み取り（`scope="shared"` 付きの `wiki_ls`、`wiki_read`、`wiki_search`、`wiki_stats`、`wiki_lint`）で見えるのは、すべての呼び出し元が見られるページだけです。`departments/<部署>/` のページや、`visible_to_departments` で限定された名前空間は見えません。

このキーは外部キーです。そのため wiki はクライアント自身のディレクトリに置かれ、クライアントが引数で別の名前空間や社員 id を指定することもできません。その代わり、外部クライアントに許されるスコープ（`memory:*`、`wiki:*`、`messaging:send`）しか持てず、`mcp init --scopes` にそれ以外を渡すと拒否されます。

これらのスコープ以外のツールは表示されず、AI 社員として動くツール（次節）も表示されません。名前を指定して呼んでもサーバーが拒否します（`-32003`）。一覧は権限の判定に従うもので、その代わりではありません。

### 含まれないもの、その理由

2026-10-07、1.70.1 のバイナリ、新しいデータディレクトリ、admin 以外の全スコープを持つキーで各ツールを 1 回ずつ呼んだ結果です。最初の 3 行はそれらのツールの元の動作で、現在サーバーは AI 社員でないすべてのキーに対して拒否します。

| スコープまたはツール | gateway なしでの結果 |
|---|---|
| `working_state_get`／`_set`／`_clear`／`_handoff` | 拒否（`-32003`）。サーバーを動かす AI 社員として動く（社員がいなければ `unknown agent: dudu`） |
| `memory_search_by_layer`、`memory_successful_conversations`、`memory_episodic_pressure`、`memory_consolidation_status` | 拒否（`-32003`）。読むのはクライアントではなく既定の社員のメモリ |
| `shared_wiki_delete`、`wiki_namespace_status`、`canvas_push`、`canvas_clear` | 拒否（`-32003`）。既定の社員として判定・動作する |
| `messaging:send`（`send_message`、`send_photo`、`send_sticker`、`synthesize_speech`、`transcribe_audio`） | `config.toml` のチャネル設定（`Unknown channel`）または外部の音声サービスが必要 |
| `mail:read`／`mail:send` | 外部キーには付与できない。`mail_*` は上の行と同じく拒否される。メール処理は gateway 内で動く |
| `team:handoff` | 外部キーには付与できない。gateway の goal loop が出したタスクが必要 |
| `odoo:*`、`notion:*`、`google:*`、`github:*` | ダッシュボードでの連携設定が必要 |
| `discovery:execute` | `discovery requires an explicit signed caller identity` |
| `skill:execute`（`office_script`） | 外部キーには付与できない。AI 社員のディレクトリに書き込む |
| `identity:read`、`files:read` | 動くが外部キーには付与できない。データ（人物名簿、`~/.duduclaw/attachments`）はプラットフォームで設定する |
| `fork:execute`、`os:native`、`recording`、`db:read` | 社員ごとの機能スイッチが必要 |
| その他（`tasks_*`、`web_fetch_cached`、`agent_*` など） | `Insufficient scope: Admin required` |

## フルプラットフォームへの移行

`duduclaw run` は同じデータディレクトリで gateway とダッシュボードを起動し、単体モードのキーはそのまま使えます。引き継がれるものと、そうでないもの：

- AI 社員は単体モードのメモリを見ません。社員は自分の名前空間（社員 id）を読み、単体モードのクライアントは `external/standalone-<クライアント>` を読みます。
- 単体モードの wiki は `agents/standalone-<クライアント>/wiki/` に残ります。このディレクトリには `agent.toml` がないので、gateway は社員ではなく保存場所として扱います。MCP 設定は書き込まず、`standalone-` で始まる名前の社員は作成できません。`wiki_share` で共有したページは共有 wiki にあり、社員も読めます。
- gateway 専用のツールは社員向けで、単体モードのキーには表示されません。クライアントにもっと使わせるには `duduclaw mcp issue-refresh-token` で別のキーを発行してください。

## トラブルシューティング

- `MCP authentication failed: DUDUCLAW_MCP_API_KEY environment variable not set. Run: duduclaw mcp init --client claude-code`：クライアントがキーなしでサーバーを起動しました。`mcp init` を再実行するか、`env` の設定を追加してください。
- `API key not found in registry`：キーが失効済みか、サーバーが `mcp init` の書き込み先とは別のデータディレクトリを読んでいます（`DUDUCLAW_HOME` を確認）。
- `API key expired`：キーの有効期間は 90 日です。`mcp init` を再実行してください。

## MCP Registry への掲載

Registry 用メタデータは `distribution/registries/mcp/server.json` です。公開手順（オーナーが各リリース後に実行）は [`distribution/registries/README.md`](../../../distribution/registries/README.md) にあります。
