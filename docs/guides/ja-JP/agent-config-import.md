# 他のプラットフォームから AI スタッフを取り込む

ほかのプラットフォームで定義したエージェントを、そのまま DuDuClaw の AI スタッフにできます。対象は Doubao パートナーパッケージ、OpenClaw や Hermes のフォルダ、`AGENTS.md`、Claude Code のサブエージェント、GitHub Copilot のカスタムエージェント、Dify アプリのエクスポートです。人格、作業ルール、サービス対象のメモ、記憶、スキルが、DuDuClaw で実際に読まれる場所にそれぞれ入ります。

ほかの言語：[English](../agent-config-import.md)、[繁體中文](../zh-TW/agent-config-import.md)。

## 入口は二つ

**ダッシュボード**：「AI チーム」ページ →「アップロード」。`.zip`、または単体の `.md`／`.yml`／`.yaml` を選びます（上限 50 MB）。ダッシュボードが取り込むのはこのボタンでアップロードしたものだけで、サーバー上のフォルダを取り込むにはターミナルのコマンドを使います。DuDuClaw がまず検査して計画を表示します。作成されるスタッフ、各ファイルの取り込み先、無効のままになるスキルとその理由が並びます。「取り込む」を押すまで何も書き込まれません。完了後は実際の結果が表示されます。

**ターミナル**：

```bash
duduclaw pack install ./報告顧問.zip --dry-run   # 計画だけ表示、書き込みなし
duduclaw pack install ./報告顧問.zip             # 取り込み
duduclaw pack install ./reviewer.agent.md            # 単体ファイルも可
duduclaw pack install ~/.openclaw                     # フォルダも可
```

同じ ID のスタッフがすでにいる場合、`--rename` を付けると末尾に `-imported` を付けて取り込みます。付けない場合は競合としてレポートに載り、そのスタッフはスキップされます。`duduclaw pack install` はエキスパートパックと同じコマンドで、取り込み専用のコマンドはありません。チャネルのトークンやスケジュールも含めてプラットフォームごと移すときは、引き続き `duduclaw migrate from openclaw|hermes` を使います（[migrate-from.md](migrate-from.md) を参照）。こちらも下の表と同じ配置になりました。

## 対応フォーマット

| フォーマット | 判定方法 | スタッフ 1 人に対応する単位 |
|---|---|---|
| Doubao パートナーパッケージ（詳細ページ「…」› パートナーパッケージを書き出す） | `units` を持つ `manifest.json`、または `agents/<ref>/agent.yml` と `persona/` | `agents/<ref>/` |
| OpenClaw フォルダ | `openclaw.json`（`moltbot.json`、`clawdbot.json` も可） | `agents.list[]` または `agents.entries` の 1 項目 |
| OpenClaw／SoulSpec のペルソナワークスペース | 最上位に `SOUL.md` | フォルダ |
| Hermes フォルダ（`~/.hermes` または 1 つの profile） | `config.yaml` と `SOUL.md` または `memories/` | フォルダ |
| Dify アプリのエクスポート | `app:` と `kind: app`（または `version`）を持つ `.yml` 1 つ | ファイル |
| Claude Code サブエージェント、Copilot カスタムエージェント | `.claude/agents/*.md`、`.github/agents/*.md`、`*.agent.md`、`*.chatmode.md`、または frontmatter に `name` と `description` がある `.md` | ファイル |
| 指示ファイル | `AGENTS.md`、`CLAUDE.md`、`GEMINI.md`、`AGENT.md`、`.github/copilot-instructions.md` | フォルダ |

エキスパートパック（`expert.toml`）、Claude Code プラグイン、単体の Agent Skill はこれまでどおり使えます（[エキスパートパック](../../features/ja-JP/32-expert-packs.md)）。それ以外はアップロードの中身を一覧にして拒否します。途中まで取り込むことはありません。

## 各ファイルの取り込み先

| 元のファイル | 取り込み先 | 補足 |
|---|---|---|
| `SOUL.md`、Doubao の `persona/SOUL.md` | `SOUL.md` | スタッフは読むだけ。 |
| `IDENTITY.md` | `IDENTITY.md`、名前と絵文字は `agent.toml` にも | OpenClaw の `- Name:`／`- Emoji:` 行と Doubao の「## 簽名」節の両方を読みます。 |
| `AGENTS.md`、`CLAUDE.md`、`GEMINI.md`、サブエージェント本文、Dify の `pre_prompt` | `INSTRUCTIONS.md`（新規） | 毎回のプロンプトで SOUL、IDENTITY の後に入ります。スタッフは読むだけ。 |
| `USER.md`（Hermes：`memories/USER.md`） | ナレッジページ「服務對象」 | コア層なので毎回のプロンプトに入ります。 |
| `TOOLS.md` | ナレッジページ「工具備註」 | メモだけで、ツール権限は付与しません。 |
| `HEARTBEAT.md` | ナレッジページ「定期檢查清單」 | 自動ではスケジュールされません。必要ならダッシュボードで作成します。 |
| `BOOT.md`、Dify の開始メッセージ | ナレッジページ | 必要なときに検索されます。 |
| `MEMORY.md`、`memory/*.md`（Hermes：`memories/MEMORY.md`） | 記憶。箇条書き 1 項目（または段落、`§` 区切り）ごとに 1 件 | 信頼度は低め（出所は「取り込み」）。元ファイルを記録するので、後からファイル単位で取り消せます。 |
| `skills/<名前>/SKILL.md` | スタッフのスキル | 下の「スキル」を参照。 |
| サブエージェント内の MCP サーバー | 取り込まない | サーバーは DuDuClaw が実行または接続するものなので、取り込みでは追加しません。一覧に出るので、信頼できるものだけダッシュボードで設定してください。 |
| `model` | `[model] preferred` | Claude のモデル ID のみ。それ以外（`doubao-*`、`gpt-*` など）はレポートに記載し、既定のモデルを使います。 |
| `tools`／`disallowedTools` | `allowed_tools`／`denied_tools` | 絞り込みのみ。DuDuClaw が知らない名前は捨てて一覧にします。許可リストには DuDuClaw 自身のツール（`mcp__duduclaw__*`：ナレッジ、記憶、タスク、委任）を必ず残します。元でツールを制限していたのに名前が一つも対応しない場合は、全開放にせず、コマンド実行・ファイル書き込み・Web 取得をオフにします。 |
| `BOOTSTRAP.md`、`CLAUDE.local.md` | スキップ | 初回だけの設定と個人用ファイル。 |
| その他 | `<home>/imported/<ID>/source/` に保存 | レポートに一覧が出ます。保存するのは取り込みで読んだファイルと一覧に出たファイルだけで、認証情報ファイル、プラットフォームの設定ファイル（`openclaw.json`、Hermes の `config.yaml`）、会話ログ、隠しフォルダは保存しません。 |

指示ファイルが複数ある場合は、`AGENTS.md` → `CLAUDE.md` → `GEMINI.md` → `AGENT.md` → `.github/copilot-instructions.md` の順で最初のものを使い、残りはスキップとして載せます。中身が `@AGENTS.md` だけのファイルは、参照先を読みます。

スタッフ ID は元の名前の英数字表記です。英数字にならない名前（中国語のパートナー名など）は「フォーマット-ref」、たとえば `doubao-a1` になります。表示名は元の名前のままです。チャネルでは「@表示名」と「@ID」でスタッフを呼ぶため、新しいスタッフがチームの誰かがすでに使っている名前・呼び出し語・ID を使うことはありません。重なったときは ID に `-imported`、表示名に「（ID）」を付け、レポートに記載します。

## スキル

スキルはすべて正しい Agent Skill に整えます。名前は小文字の英数字とハイフン（フォルダ名と同じ）になり、元の名前（多くは中国語）は `metadata.label` に残します。中国語の名前しかないスキルは `skill-<16 進 8 桁>` になります。Doubao のエクスポートによくある壊れた frontmatter は置き換え、本文はそのまま残します。

次のスキルは管理者が承認するまで**無効**のままです。

- 実行可能なスクリプトやプログラムを含む（`.py`、`.sh`、`.js` など、`#!` で始まるファイル、実行ビットの付いたファイル、画像と PDF 以外のバイナリ。`.svg`、`.html`、`.vbs`、`.wsf`、`.command` も含む）
- DuDuClaw にないツールに依存する：Feishu／Lark の文書と API、「構建網頁應用」などの Doubao 内蔵ツール、Coze プラグイン、Microsoft 365 Graph

これらはスタッフのフォルダの外で待機し、取り込み時にダッシュボードの受信箱へ承認依頼が 1 件届きます。依頼には各スキルの内容の指紋が記録され、依頼後に中身が変わったスキルは有効になりません。承認後に AI チームのカードで「適用」を押す（または `duduclaw expert skills <ID>` を実行する）とスキルが追加されます。同名のスキルがある場合は上書きしません。却下や期限切れのときは無効のままです。こうしたツールに少し触れているだけのスキルは通常どおり入り、レポートで注意を促します。

SKILL.md が内容検査に通らない、またはテキストの添付ファイルがプロンプトインジェクション検査に通らないスキルは取り込みません。

## 安全面

- スタッフは後から SOUL と INSTRUCTIONS を編集できないため、取り込み時が唯一の確認の機会です。この二つを含む取り込んだテキストはすべてプロンプトインジェクション検査を通り、SOUL と INSTRUCTIONS はスキルのセキュリティ検査も通ります。SOUL が通らなければそのスタッフは取り込まず、指示ファイルが通らなければ書き込まずにレポートに載せます。
- 認証情報は取り込みません。`.env`、`auth.json`、`auth-profiles.json` などは読まず保存もしません。`openclaw.json` のチャネルトークンはここでは扱わず、Dify の秘密変数は捨てます。
- アップロードしたフォルダの外を指す OpenClaw のワークスペースパスはたどりません。
- 圧縮ファイル：`__MACOSX/`、`._*`、`.DS_Store`、`Thumbs.db`、`desktop.ini` はスキップします。ファイル名はまず UTF-8 として読み（フラグ付き、または macOS のようにフラグなし）、中国語版 Windows で作られたものは GBK か Big5 を試します。どれも合わないとき、または GBK と Big5 が同じくらいありそうなときは、文字化けした名前で書き込まず、元のバイト列をメッセージに出して拒否します。シンボリックリンクは展開しません。ファイル名はどのシステムでも同じ意味になる必要があるため、`:` を含む名前、末尾のドットや空白、Windows のデバイス名（`CON`、`NUL`、`COM1` など）、大文字小文字だけが違う二つの項目は拒否します。エキスパートパックと同じサイズ、項目数、圧縮率の上限が適用されます。
- スキル内の認証情報ファイルと隠しファイル（`.env`、`token.json`、`*.pem`、`credentials/`、ドットで始まるファイル）はコピーせず、一覧に出します。
- AI スタッフは取り込みを実行できません。`duduclaw pack install` と `duduclaw expert skills` はスタッフのセッション内では実行を拒否します。
- スタッフは Write、Edit、MultiEdit、Bash、`office_script` で自分の `INSTRUCTIONS.md` を書き換えられません。SOUL.md や CONTRACT.toml と同じ保護です。

## 元に戻す

AI チームページから取り込みを削除する（または `duduclaw expert remove <ID>`）と、作成されたスタッフ、待機中のスキル、`<home>/imported/<ID>/` の元ファイルが消えます。取り込んだ記憶は共有の記憶ストアにあるので、レポートの「詳細」に表示されるコマンドで取り消します（取り込み元の ID にはこのコンピューター上のパスは含まれません）。

```bash
duduclaw memory forget-source --agent <ID> --session "import:agent-import:<import id>/MEMORY.md"
```

## 既知の制限

- Doubao のパッケージには公開仕様がありません。取り込みは手元の実サンプル 1 件に合わせて作っており、知らない manifest の項目やユニット種別は推測せずレポートに載せます。
- Dify のワークフロー図は変換しません。最初の LLM ノードのシステムプロンプトだけを使うので、フローは DuDuClaw で作り直してください。Dify のナレッジベースの中身はエクスポートに含まれないため、再アップロードが必要です。
- ルールフォルダ（`.cursor/rules`、`.claude/rules`、`.agents/rules`、Windsurf、Cline、Kiro）は一覧に出るだけで、まだ適用しません。
- レポートの各行はダッシュボードの言語で表示されます。「詳細」にある取り込み側の技術的な説明（ファイル名、元のプラットフォームの理由）は繁体字中国語のままです。
