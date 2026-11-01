# OpenClaw / Hermes / paperclip からのスムーズな移行

`duduclaw migrate from` は、既存の OpenClaw、Hermes、paperclip の設定を
DuDuClaw に一発で移行するコマンドです。デフォルトは**プレビューモード**で、
「何をインポートし、何をスキップし、なぜか」を表示するだけです。計画に問題が
なければ `--apply` を付けて実際に書き込みます。

旧い綴り `duduclaw migrate-from` は v1.69.0 で削除されました。実行すると新しい綴りを表示して終了コード 2 で終了します。

```bash
# プレビュー（ファイルは一切書き込まない）
duduclaw migrate from openclaw

# 計画を確認したうえで実際に適用
duduclaw migrate from openclaw --apply --confirm-profile <preview-digest>
```

## コマンド

```
duduclaw migrate from <openclaw|hermes|paperclip|claude-code> [--source <path>] [--apply] [--confirm-profile <digest>] [--rename] [--agent <id>] [--no-redact] [--json]
```

| フラグ | 動作 |
|---|---|
| （なし） | 移行計画をプレビューするのみ。ファイルは書き込まない。 |
| `--source <path>` | 移行元ディレクトリを指定。openclaw/hermes にはデフォルト値あり。**paperclip では必須**。 |
| `--apply` | 実際に書き込みを実行する。 |
| `--rename` | 同名の agent がある場合、スキップせず `-imported` サフィックス付きでインポートする。 |
| `--confirm-profile <digest>` | OpenClaw、Hermes、paperclip の `--apply` に必須。プレビューの確認ダイジェストを使用。`claude-code` には不要。 |
| `--agent <id>` | 既存の移行先スタッフを指定。`claude-code` では必須。 |
| `--no-redact` | `claude-code` の会話記録で既定の個人情報マスキングを無効にする。 |
| `--json` | 通常の表示の代わりに、機械可読な単一の JSON 結果を出力。 |

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
duduclaw migrate from openclaw            # デフォルトの移行元は ~/.openclaw
duduclaw migrate from openclaw --source /path/to/.openclaw --apply --confirm-profile <preview-digest>
```

`openclaw.json`（JSON5）を読み込み、以下をインポートします。

- **Agents**：`agents.list[]`（未指定の場合は単一の `main`）。それぞれの workspace
  ファイルは [AI スタッフの取り込み](agent-config-import.md#各ファイルの取り込み先) と同じ配置になる：
  `SOUL.md` → SOUL、`IDENTITY.md` → IDENTITY（名前と絵文字も）、`AGENTS.md` → `INSTRUCTIONS.md`、
  `USER.md` → 利用者データ、`TOOLS.md`／`HEARTBEAT.md` → ナレッジページ、`MEMORY.md` と `memory/*.md` → 記憶（元ファイルを記録）。
  1.76 より前はペルソナファイルが `agents/<id>/memory/` にコピーされるだけで、どこからも読まれていなかった。
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
duduclaw migrate from hermes --apply --confirm-profile <preview-digest>
# active ではない profile を移行する場合：
duduclaw migrate from hermes --source ~/.hermes/profiles/<name> --apply --confirm-profile <preview-digest>
```

Hermes はシングル agent プラットフォームなので、DuDuClaw agent を 1 つ（id は
`hermes`）生成します。インポートする内容：

- **モデル**：`config.yaml` の `model.default`。
- **チャンネルトークン**（`.env` から）：`TELEGRAM_BOT_TOKEN`、`DISCORD_BOT_TOKEN`、
  `SLACK_BOT_TOKEN` + `SLACK_APP_TOKEN`。`EMAIL_*` チャンネルは v1 では未対応のため
  `SKIPPED`。
- **Persona / 記憶**：`SOUL.md`、`memories/MEMORY.md`（記憶。`§` で項目を分割）、`memories/USER.md`（ネイティブ `USER.md`）。
- **Cron**：`cron/jobs.json`（防御的にパース）。
- 移行対象は **active な profile のみ**。それ以外の profile は `SKIPPED` として一覧
  表示され、`--source` を使って個別に移行するよう案内されます。

### paperclip：公式エクスポート経由

paperclip のデータは組み込みの PostgreSQL に格納されており、DuDuClaw はこのデータ
ベースに直接接続しません。まず paperclip 側でエクスポートしてください。

```bash
paperclipai company export <company-id> --out ./export \
  --include company,agents,projects,issues,tasks,skills

duduclaw migrate from paperclip --source ./export --apply --confirm-profile <preview-digest>
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
runtime（codex / openai_compat）にマッピングするかは人手での確認が
必要になります。DuDuClaw が代わりに推測することはありません。

## プロファイルの確認と Claude Code

OpenClaw、Hermes、paperclip の移行は Persona、User、Rules、Contract と入力検査結果をプレビューに表示します。適用には対応する `--confirm-profile <digest>` が必要で、セクションの内容が変われば再確認します。この移行ダイジェストは Persona、User、Rules、Contract の四つのセクションだけを対象とし、移行で出力する全ファイルを対象にはしません。`pack install` にもプレビューの `--confirm-profile <digest>` が必要で、こちらは書き込む全ファイルを対象にします。AI セッションからは実行できません。`USER.md` は最大 4,000 文字で、指示ではなくデータとして挿入します。

Claude Code も対応します：`duduclaw migrate from claude-code --source ~/.claude --agent <既存ID>`。プレビューの確認後に `--apply` を付けます。この経路に `--confirm-profile` は不要です。整理済みの記憶、低信頼度のナレッジページ、フィルタ済みの会話記録を既存スタッフに取り込み、新しいスタッフは作りません。逐語録の個人情報マスキングは既定で有効で、`--no-redact` で無効にできます。
