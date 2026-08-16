# OpenClaw / Hermes / paperclip からのスムーズな移行

`duduclaw migrate-from` は、既存の OpenClaw、Hermes、paperclip の設定を
DuDuClaw に一発で移行するコマンドです。デフォルトは**プレビューモード**で、
「何をインポートし、何をスキップし、なぜか」を表示するだけです。計画に問題が
なければ `--apply` を付けて実際に書き込みます。

```bash
# プレビュー（ファイルは一切書き込まない）
duduclaw migrate-from openclaw

# 計画を確認したうえで実際に適用
duduclaw migrate-from openclaw --apply
```

## コマンド

```
duduclaw migrate-from <openclaw|hermes|paperclip> [--source <path>] [--apply] [--rename]
```

| フラグ | 動作 |
|---|---|
| （なし） | 移行計画をプレビューするのみ。ファイルは書き込まない。 |
| `--source <path>` | 移行元ディレクトリを指定。openclaw/hermes にはデフォルト値あり。**paperclip では必須**。 |
| `--apply` | 実際に書き込みを実行する。 |
| `--rename` | 同名の agent がある場合、スキップせず `-imported` サフィックス付きでインポートする。 |

各項目には以下のいずれかのステータスが付きます。

- `IMPORTED` — インポート済み（または今回インポートされる）。
- `PARTIAL` — 部分的にインポート、または人手による確認が必要（例：Claude 以外のモデル）。
- `SKIPPED(理由)` — 何らかの理由でスキップ（移行元ファイル欠落、パース失敗、セキュリティによるブロックなど）。
- `CONFLICT(理由)` — 移行先にすでに値があり、既存の設定を守るため上書きしなかった。

全体の結果は `COMPLETE` / `DEGRADED` / `PARTIAL` のいずれかに集約されます。適用後は
`~/.duduclaw/imported/<platform>/migration-report.md` に完全なレポートが書き出されます。
すべてのトークン値は「先頭 4 文字＋末尾 4 文字」のマスク表示のみで、画面にもレポートにも
平文では出力されません。

## プラットフォーム別

### OpenClaw（`~/.openclaw`）

```bash
duduclaw migrate-from openclaw            # デフォルトの移行元は ~/.openclaw
duduclaw migrate-from openclaw --source /path/to/.openclaw --apply
```

`openclaw.json`（JSON5）を読み込み、以下をインポートします。

- **Agents**：`agents.list[]`（未指定の場合は単一の `main`）。それぞれの workspace
  persona（`SOUL.md`）と記憶（`MEMORY.md` / `USER.md` / `memory/*.md` の箇条書き）も
  含む。
- **チャンネルトークン**：`channels.telegram.botToken`、`channels.discord.token`、
  `channels.slack.botToken` + `appToken`（暗号化して config.toml に書き込み）。
  WhatsApp は linked-device のため技術的に移行不可能で `SKIPPED`。
- **モデル**：`agents.defaults.model.primary`（`anthropic/` プレフィックスを除去）。
- **Anthropic API key**：`env` セクションと `~/.openclaw/.env` から取得。他プロバイダーの
  キーは `SKIPPED`。
- **Cron**：旧形式の `cron/jobs.json`（防御的にパース）。新しい SQLite cron スキーマは
  未検証のため `SKIPPED`。
- **Skills**：OpenClaw の優先順位に従って `SKILL.md` フォルダを探索（先にスキャンして
  からインストール）。

旧ディレクトリ名 `~/.moltbot`、`~/.clawdbot` もサポートしています。

### Hermes（`~/.hermes`）

```bash
duduclaw migrate-from hermes --apply
# active ではない profile を移行する場合：
duduclaw migrate-from hermes --source ~/.hermes/profiles/<name> --apply
```

Hermes はシングル agent プラットフォームなので、DuDuClaw agent を 1 つ（id は
`hermes`）生成します。インポートする内容：

- **モデル**：`config.yaml` の `model.default`。
- **チャンネルトークン**（`.env` から）：`TELEGRAM_BOT_TOKEN`、`DISCORD_BOT_TOKEN`、
  `SLACK_BOT_TOKEN` + `SLACK_APP_TOKEN`。`EMAIL_*` チャンネルは v1 では未対応のため
  `SKIPPED`。
- **Persona / 記憶**：`SOUL.md`、`memories/MEMORY.md`、`memories/USER.md`。
- **Cron**：`cron/jobs.json`（防御的にパース）。
- 移行対象は **active な profile のみ**。それ以外の profile は `SKIPPED` として一覧
  表示され、`--source` を使って個別に移行するよう案内されます。

### paperclip：公式エクスポート経由

paperclip のデータは組み込みの PostgreSQL に格納されており、DuDuClaw はこのデータ
ベースに直接接続しません。まず paperclip 側でエクスポートしてください。

```bash
paperclipai company export <company-id> --out ./export \
  --include company,agents,projects,issues,tasks,skills

duduclaw migrate-from paperclip --source ./export --apply
```

`--source` は**必須**です（指定しない場合は上記の手順が表示されます）。インポート
する内容：

- **Agents**：`agents/<slug>/AGENTS.md` の frontmatter（`name/title/reportsTo/skills`）
  → DuDuClaw agent、本文 → `SOUL.md`。`reportsTo` はそのまま `reports_to` に
  マッピングされる（作成時は上位・下位のトポロジー順にソート。循環を検出した場合は
  全員を上位なしに変更し `PARTIAL` を付与）。
- **Tasks**：`tasks/<slug>/TASK.md` → Task Board。`recurring` → cron。
- **Skills**：`skills/<slug>/SKILL.md` → agent の SKILLS/（先にスキャン）。
- **COMPANY.md** → 共有 wiki ページ `shared/wiki/imported/paperclip-company.md`。
- paperclip の公式エクスポート形式には**機密情報が含まれない**（channel token /
  API key / DB id）ため、チャンネルとキーは常に `SKIPPED`。

## セキュリティとデータ保全

- **Skills は先にスキャンしてからインストール**：`SKILL.md` は必ず duduclaw-security
  の prompt-injection スキャナー（ルール 6 種類）を通過する。検出時は fail-closed で
  インストールされず `SKIPPED(security)` になる。インポートされた skill は
  `skill_auto_activate` を `false`（安全側のデフォルト）のまま維持する。
- **絶対に上書きしない**：既存の同名 agent は `SKIPPED`（または `--rename` を使う）。
  config.toml にすでにある channel token / API key は `CONFLICT` になり、元の値は
  変更されない。
- **トークンは暗号化して保存**：channel token と API key は AES-256-GCM で暗号化して
  から config.toml に書き込まれ、平文でファイルに残ることはない。
- **データは失われない**：v1 では会話履歴を `sessions.db` にパースして取り込むことは
  しないが、`--apply` は元の session / 会話ファイルをそのまま
  `~/.duduclaw/imported/<platform>/raw/` にアーカイブし、後から参照できるようにする。

## v1 の非対応範囲（正直な境界線）

1. 会話履歴のデータベース化（そのままのアーカイブのみ）。
2. OpenClaw の新しい SQLite cron / auth-profiles（スキーマ未検証）。
3. WhatsApp の linked-device 認証情報（デバイスに紐づいており移行不可）。
4. Hermes の active 以外の profile（`--source` を使って個別に移行可能）。
5. paperclip の Postgres への直接読み込み（公式エクスポートを利用すること）。
6. 外部記憶バックエンド（Honcho / Mem0 / QMD / LanceDB）。

## よくある質問

**Q：プレビューは何かを変更しますか？**
いいえ。`--apply` を付けない限り、ファイルは一切書き込まれず、チャンネルも起動され
ません。

**Q：途中で CONFLICT が出た場合はどうすればよいですか？**
CONFLICT は移行先にすでに値があり、既存の設定を守るためにスキップされたことを意味
します。インポートした値に置き換えたい場合は、config.toml 内の古い値を手動で削除
してから再実行するか、`--rename` を使って別の agent としてインポートしてください。

**Q：Claude 以外のモデルはどうなりますか？**
`[model] preferred` にそのまま保持され `PARTIAL` としてマークされます。どの
runtime（codex / gemini / openai_compat）にマッピングするかは人手での確認が
必要になります。DuDuClaw が代わりに推測することはありません。
