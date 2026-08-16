# 同一マシンで複数の DuDuClaw インスタンスを実行する（Plan A）

各インスタンスに専用の**状態ルート**・**ポート**・**インスタンス名**を与えれば、一つの binary を共有したまま、同一マシン上で複数の独立した DuDuClaw インスタンスを実行できます。これが「Plan A」、最も軽量な分離モデルです。より強い分離（独立した OS ユーザー、またはコンテナ）が必要な場合は、末尾の代替案を参照してください。

## 三つの環境変数

| 環境変数 | 用途 | インスタンスごとに変える必要 |
| --- | --- | --- |
| `DUDUCLAW_HOME` | 状態ルート：config、SQLite データベース、`bus_queue.jsonl`、`events.db`、models、shared wiki、secrets、cron。デフォルトは `~/.duduclaw`。 | **あり** |
| `DUDUCLAW_PORT` | Gateway の HTTP/WS ポート。デフォルトは `18789`。 | **あり** |
| `DUDUCLAW_INSTANCE` | 短いインスタンス名（`[a-z0-9-]`）。`~/.claude/settings.json` 内の**グローバル MCP 登録**キーに名前空間を付け（`duduclaw` → `duduclaw-<name>`）、インスタンス同士が互いを上書きしないようにする。 | 推奨 |

各サブシステムは単一の標準ヘルパー（`duduclaw_core::duduclaw_home()`）を通じて自身の状態ルートを解決するため、`DUDUCLAW_HOME` を設定すると、そのインスタンスに属する*すべて*の状態が移動します。パスが `~/.duduclaw` へこっそり漏れ戻ることはありません。

## 例：2つのインスタンス

```bash
# インスタンス "work"
DUDUCLAW_HOME=~/dd-work  DUDUCLAW_PORT=18789 DUDUCLAW_INSTANCE=work \
  duduclaw run --yes

# インスタンス "play"
DUDUCLAW_HOME=~/dd-play  DUDUCLAW_PORT=18790 DUDUCLAW_INSTANCE=play \
  duduclaw run --yes
```

各インスタンスが自分の MCP server を登録する際、共有の `~/.claude/settings.json` に名前空間付きのエントリを書き込み、自身の環境変数を起動仕様に含めます。これにより、Claude CLI が起動した `duduclaw mcp-server` が正しいインスタンスへ接続できます：

```jsonc
{
  "mcpServers": {
    "duduclaw-work": {
      "command": "/path/to/duduclaw",
      "args": ["mcp-server"],
      "env": { "DUDUCLAW_HOME": "/Users/you/dd-work", "DUDUCLAW_PORT": "18789", "DUDUCLAW_INSTANCE": "work" }
    },
    "duduclaw-play": {
      "command": "/path/to/duduclaw",
      "args": ["mcp-server"],
      "env": { "DUDUCLAW_HOME": "/Users/you/dd-play", "DUDUCLAW_PORT": "18790", "DUDUCLAW_INSTANCE": "play" }
    }
  }
}
```

`"command"` には `duduclaw` binary の絶対パスを指定します。`which duduclaw` で調べられます（npm のグローバル bin ディレクトリ、あるいはデスクトップアプリに同梱された binary など）。

## 必ず変える項目チェックリスト

- [ ] `DUDUCLAW_HOME`：インスタンスごとに異なるディレクトリ
- [ ] `DUDUCLAW_PORT`：インスタンスごとに異なるポート（`http-server --bind` を動かす場合は MCP HTTP ポートも）
- [ ] `DUDUCLAW_INSTANCE`：インスタンスごとに異なる名前（MCP 登録の名前空間に使われる）
- [ ] launchd／systemd の**サービスラベル**：インスタンスごとに異なる値
- [ ] **models ディレクトリ**：すべての `DUDUCLAW_HOME/models` を一つの共有・読み取り専用の場所（symlink）に向け、数 GB の GGUF モデルファイルを重複保持しないようにする

## 共有される状態と分離される状態

- **`DUDUCLAW_HOME` によって分離**：config、すべての SQLite データベース、bus queue、events、cron、shared wiki、JWT／keyfile、evolution の状態。
- **同一 OS ユーザー配下で共有**：`~/.claude`（Claude CLI の OAuth セッションと MCP 設定）。インスタンスは名前空間付きの MCP キーによってここで共存できますが、依然として**同じ OAuth サブスクリプションアカウント**を使うため、重い同時利用はローテーションや rate-limit の競合を引き起こすことがあります。各インスタンスの `config.toml` に専用のアカウントを設定するか、アカウントごとの profile（`~/.claude/profiles/<name>`）を使うことで干渉を避けられます。

## より強い分離モデルを選ぶべきとき

- **独立した OS ユーザー**：各インスタンスが自分のアカウント配下で動くため、`~/.duduclaw` と `~/.claude`（OAuth）はファイルシステムレベルの境界で自然に分離されます。環境変数への依存はゼロですが、ポートは依然として個別に必要です。
- **コンテナ（Docker／Podman）**：ファイルシステムとネットワーク名前空間の完全な分離。各コンテナは内部で同じポート `18789` を再利用し、ホスト側の異なるポートへマッピングできます。注意：macOS 上では Linux コンテナに Metal がないため、ローカルの GGUF 推論は CPU にフォールバックします（GPU が必要な場合は推論をホスト側に残してください）。
