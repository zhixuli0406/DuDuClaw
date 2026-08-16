# 専門ソフトウェア統合:Photoshop と AutoCAD

本ガイドでは、DuDuClaw の AI 社員に Adobe Photoshop と AutoCAD を操作させる方法を説明します。コミュニティが保守する MCP server を per-agent の `.mcp.json` で組み込み、ケイパビリティガバナンスで高リスクなツールを囲う——DuDuClaw 自身はこれらのソフトウェアのドライバーを書きません。この構成なら、専門ソフトウェアを1つ追加するのに DuDuClaw の新バージョンを待つ必要はなく、MCP server を差し替えるだけでツールセットが入れ替わります。

> この2つの MCP server はいずれもサードパーティ製の非公式プロジェクトです(いずれも MIT ライセンス)。両者が公開する任意スクリプト実行インターフェース(Photoshop の ExtendScript、AutoCAD の AutoLISP)は、本質的にリモートコード実行(RCE)面にほかなりません。本ページの「リスク開示」と「ケイパビリティガバナンス」の2節を必ず先に読んでから導入してください——ガードレールなしに素のままインストールして使わないこと。

## サポートマトリクス

| ソフトウェア | MCP server | プラットフォーム | 接続方式 | 要件 | RCE 面 |
|------|-----------|------|----------|------|--------|
| Photoshop | `@alisaitteke/photoshop-mcp`(npm) | macOS + Windows | macOS は AppleScript / Windows は COM、ExtendScript API に統一 | Photoshop インストール済み(2012–2025+)、Node.js(`npx`) | `photoshop_execute_script`(任意の ExtendScript) |
| AutoCAD(File IPC) | `puran-water/autocad-mcp`(Python) | **Windows 限定** | temp ファイル + `PostMessageW` 注入 + AutoLISP dispatcher | Windows 10/11、AutoCAD LT 2024+、Python(`uv`) | `system` ツール内の `execute_lisp`(任意の AutoLISP) |
| AutoCAD(ezdxf) | 同上、`AUTOCAD_MCP_BACKEND=ezdxf` | **クロスプラットフォーム**(Win/mac/Linux/WSL) | headless で DXF を直接読み書き、AutoCAD プロセスには触れない | Python(`uv`)、**AutoCAD 不要** | **なし**(AutoLISP を実行しない) |

要点:クロスプラットフォーム、バッチ処理、無人運用、または出所が不確かな CAD 作業は、必ず **ezdxf** バックエンドを使うこと。AutoCAD を起動せず AutoLISP も実行しないため、RCE 面はゼロになります。File IPC は「人が付き添っていて、本物の AutoCAD 幾何エンジンが必要」な場合にのみ使ってください。

## インストール手順

### 1. 対象ソフトウェアと MCP ランタイムを用意する

- **Photoshop**:ローカルに Adobe Photoshop をインストールし、起動できることを確認します。コア機能に UXP プラグインは不要です——neural filters(肌のレタッチ、色付け)だけがオプションの UXP ブリッジを必要とします。ほかに Node.js(`npx` 用)が必要です。
- **AutoCAD**:`puran-water/autocad-mcp` を clone し、`uv sync` で依存関係をインストールします。clone 後は commit hash を確認してください、`uv.lock` が依存バージョンを固定します。File IPC バックエンドはさらに Windows + AutoCAD LT 2024+ が必要で、upstream の説明に従って `mcp_dispatch.lsp` を読み込む必要があります。

### 2. agent の `.mcp.json` に server を組み込む

MCP server は全体設定ではなく、**単一の agent** の `.mcp.json`(`~/.duduclaw/agents/<id>/.mcp.json`)に組み込みます。このファイルには agent のインストール時点で `duduclaw` server のエントリがすでに書き込まれています(agent が DuDuClaw 自身の MCP ツールにアクセスするために必須のものです)。新しい server を組み込むときは `mcpServers` に**マージ**し、既存の `duduclaw` エントリを残してください——ファイル全体を上書きしないこと。

Photoshop(テレメトリを無効化——「リスク開示」参照):

```json
{
  "mcpServers": {
    "duduclaw": { "command": "…", "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "<id>" } },
    "photoshop": {
      "command": "npx",
      "args": ["-y", "@alisaitteke/photoshop-mcp"],
      "env": { "LOG_LEVEL": "2", "ANALYTICS_DISABLED": "1", "POSTHOG_DISABLED": "1" }
    }
  }
}
```

AutoCAD(`command` はローカル venv の絶対パスに書き換える):

```json
{
  "mcpServers": {
    "duduclaw": { "command": "…", "args": ["mcp-server"], "env": { "DUDUCLAW_AGENT_ID": "<id>" } },
    "autocad-mcp": {
      "command": "C:\\path\\to\\autocad-mcp\\.venv\\Scripts\\python.exe",
      "args": ["-m", "autocad_mcp"],
      "env": { "AUTOCAD_MCP_BACKEND": "auto" }
    }
  }
}
```

`serverKey`(`photoshop` / `autocad-mcp`)はツール名のプレフィックスになります:Claude 側では、ツールは `mcp__<serverKey>__<tool>` という名前になります。次節のケイパビリティ設定はこの命名を使います。

### 3. ケイパビリティガバナンスを設定する

次節を参照してください。これは必須の手順であり、任意ではありません。

> 前述の2つの専門ソフトウェアには、対応する有料エキスパートパック(`marketing-designer` マーケティングデザイナー、`cad-drafter` CAD 製図員)がそれぞれ用意されており、soul、`.mcp.json` テンプレート、ケイパビリティ設定、安全 SOP をひとまとめにして一度のインストールで完了します。これらの単体エキスパートパックは、ダッシュボードのエキスパートパックページの組み込みカタログ(職能部門別に表示)にも並びます。インストール時に「レポート先」メニュー(または CLI の `duduclaw expert install <pack> --attach-under <agent-id>`)を使えば、既存の管理者の下にエキスパートを直接組み込み、組織図と部門にそのまま組み入れられます。

## ケイパビリティガバナンス

DuDuClaw は agent の `agent.toml [capabilities]` でツールを制御します。以下の4つのフィールドが、こうした外部 MCP server を統制する主力であり、いずれも既存の仕組みです——設定を正しく書けばそのまま有効になります。

### `allowed_tools`:有効化スイッチ(未設定だと静かに無効化される)

外部の per-agent MCP ツールは Claude CLI のデフォルト allow-list に**含まれません**。`-p` サブプロセスモードでは、`allowed_tools` に含まれないツールはすべて対話的な確認を必要としますが、サブプロセスには確認する手段がなく、結果として**サイレントな no-op** になります。Photoshop/AutoCAD のツールを使えるようにするには、`allowed_tools` に `mcp__photoshop__*` または `mcp__autocad-mcp__*` を明示的に含め、agent が引き続き必要とする他のツールも合わせて補う必要があります:

```toml
[capabilities]
allowed_tools = [
  "mcp__duduclaw__*", "mcp__photoshop__*",
  "WebSearch", "WebFetch", "Read", "Write", "Edit", "Glob", "Grep", "TodoWrite",
]
```

`allowed_tools` を設定した時点で、それが**唯一**の自動承認セット(allowlist モード)になり、リストにないツールは一律に許可されません——これは攻撃対象領域を狭めるうえで有利に働きます(たとえば意図的に `Bash` をリストから外すなど)。

`allowed_tools`/`denied_tools` は現在、独立した2層で強制されます:上で述べた Claude CLI `-p` サブプロセスの allow-list が管理するのは**この agent が CLI 経由で spawn する呼び出し**だけです。MCP ディスパッチゲート(`McpDispatcher`)は別途、**MCP server と直接やり取りするすべての呼び出し**(stdio/HTTP/SSE、または Claude 以外のランタイムの openai-compat tool-loop)に対して同じ設定をもう一度チェックし、ツールのベース名を正確に照合します(`mcp__<server>__` プレフィックスは自動的に取り除かれます)。両層は同一の設定を同一のロジックで判定するため(`denied_tools` が常に優先)、CLI 以外の呼び出し経路をカバーするために別の設定を用意する必要はありません。

### `denied_tools`:RCE ツールを強制ブロックする(評価で必ず勝つ)

`denied_tools` は `allowed_tools` の後に評価され、常に優先されます。Photoshop の任意スクリプトツールはこれで強制ブロックします:

```toml
denied_tools = ["mcp__photoshop__photoshop_execute_script"]
```

### `scoped_tools`:高リスクなツールを人による許可ゲートに通す

`scoped_tools` に列挙されたツールは、有効な許可(grant)がない限り Claude CLI の `--disallowedTools` に組み込まれます。使うにはタスクごとに `capability_request` → ApprovalBroker を通じて人が承認する必要があり(PORTICO task-scoped grant)、タスク終了と同時に取り消されます。上書き保存系のツールや、`denied_tools` では正確に切り分けられない RCE に使います。

AutoCAD の `execute_lisp`(RCE)は upstream で `system` ツール内に、undo/redo/screenshot と同じツールとして組み込まれています。危険な部分操作だけをブロックすることはできないため、`system` ツール全体を許可ゲートに通します:

```toml
scoped_tools = ["mcp__autocad-mcp__system"]
```

Photoshop の上書き保存系のツールも同様に許可ゲートを通します(実際のツール名は server の初回 introspection で確認してください):

```toml
scoped_tools = ["mcp__photoshop__photoshop_save_document", "mcp__photoshop__photoshop_close_document"]
```

### `maybe_irreversible_tools`:ActionGuard のオーバーライドヒント

「不可逆かもしれない」と印を付けたツールは、goal-loop / duduclaw-dispatch 経路を通るとき LLM judge または人による確認に回されます。これは補完的な宣言であり、素の `-p` CLI ターン内の外部 MCP ツールについては、依然として主に `scoped_tools` が `--disallowedTools` に組み込むことで封じ込めを担います。

### 粒度の限界(正直な注意点)

ケイパビリティガバナンスの粒度は「ツール」単位までです。upstream が危険な操作と安全な操作を同じツールにまとめている場合(AutoCAD の `system` は `execute_lisp`、undo、screenshot を同時に含む)、危険な部分操作だけをブロックすることはできず、ツール全体を許可ゲートに通すしかありません(代償として、安全な部分操作にも許可が必要になります)。よりきれいな分離が必要なら、RCE を含まないバックエンドに切り替えてください(AutoCAD なら ezdxf)。

## リスク開示

- **任意コード実行(RCE)**:ExtendScript も AutoLISP も、任意のファイルを読み書きし、外部プログラムを起動できます——デスクトップユーザー権限の shell と同等です。これは2つの自動化エコシステムの設計であり、bug ではありません。前節の通り必ず強制ブロックする(Photoshop)か許可ゲートに通し(AutoCAD)、agent に対して全開放しないでください。
- **テレメトリ(Photoshop)**:`@alisaitteke/photoshop-mcp` はデフォルトでサードパーティのテレメトリ(Mixpanel / PostHog)が有効になっており、使用イベントを送信します。組み込む際は `.mcp.json` の `env` に `ANALYTICS_DISABLED=1` を指定してください(本ページの例にはすでに含まれています)。
- **サプライチェーン**:`npx -y @alisaitteke/photoshop-mcp` は毎回 npm の最新版を取得し、`-y` が自動的に同意します。本番投入前にクリーンな環境で特定バージョンの挙動を検証したうえで、`args` を `@alisaitteke/photoshop-mcp@<バージョン>` に固定してください。AutoCAD 側は clone 後に commit を確認し、`uv.lock` に依存します。導入前に `npm audit` / `pip-audit` を実行することを推奨します。
- **非公式**:いずれも Adobe / Autodesk の公式プロジェクトではなく、開発元との提携もありません。
- **1行ずつのソースコード監査は未実施**:上記は両 repo の README(一次情報源)に基づいて確認したものです。README が開示していない部分(たとえば Photoshop の「API key はローカルから出ない」という主張の実際の送信挙動)は未検証であり、本番導入前にソースコードレベルの監査と依存関係スキャンを補うことが望まれます。

## トラブルシューティング

| 症状 | 想定される原因 | 対処 |
|------|----------|------|
| Photoshop/AutoCAD のツールを「呼んでも反応がない」 | `allowed_tools` にその server の `mcp__<key>__*` が含まれていない | 追加してください(「有効化スイッチ」参照)。外部 MCP ツールはデフォルトの allow-list に含まれません |
| 保存系のツールが常にブロックされる | `scoped_tools` にあり、有効な許可がない | 想定どおりの挙動です。上書きするには先に ApprovalBroker の承認を得てください |
| AutoCAD のツールがまったく反応しない | `.mcp.json` の `command` がまだテンプレートのパスのまま | ローカル venv の python の絶対パスに変更してください |
| クロスプラットフォームで File IPC が動かない | File IPC は Windows + AutoCAD LT 2024+ のみ対応 | `AUTOCAD_MCP_BACKEND=ezdxf` に切り替えてください |
