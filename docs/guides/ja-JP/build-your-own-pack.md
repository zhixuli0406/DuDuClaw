# エキスパートパックを自作する(ハンズオンチュートリアル)

> 対象:「1人のAI社員」または「1つのチーム」を配布可能なインストールパッケージにまとめたい制作者(SI、コンサルタント、コミュニティ貢献者)向け。
> これはチュートリアルです。フィールドの完全なリファレンスは[features/32-expert-packs.md](../../features/ja-JP/32-expert-packs.md)を参照してください。

エキスパートパックはDuDuClawエコシステムの統一パッケージング単位です:ディレクトリ(またはzip/URL)の中に、社員のペルソナ、スキル、SOP wikiページ、推奨プロンプトが入っており、相手は1行のコマンドで自分のDuDuClawにインストールできます。

## 1. 最小構成のパック(10分)

ディレクトリを作成します:

```
my-first-pack/
├── expert.toml
├── agents/
│   └── helper/
│       ├── soul.md              # 社員のペルソナ(アイデンティティ/責務/境界)
│       └── agent.partial.toml   # 任意:agent.tomlにディープマージされるフラグメント
└── skills/
    └── greeting/
        └── SKILL.md             # 任意:パックに同梱されるスキル
```

`expert.toml`の最小内容(フィールドは[features/32](../../features/ja-JP/32-expert-packs.md)を正としてください):

```toml
[expert]
name = "my-first-pack"
description = "デモ:フレンドリーな小さなヘルパー"
version = "0.1.0"
author = "あなたの名前"
license = "MIT"
tags = ["demo"]
category = "general"

[[expert.agents]]
name = "helper"
role = "main"
display_name = "ヘルパー"
```

`agents/helper/soul.md`にペルソナを書きます。アイデンティティ/責務/境界の3段構成が良い出発点です——境界が明確であるほど、インストールする側は安心して使えます。

## 2. ローカルテストループ

```bash
# 検証 + インストール(ディレクトリから直接インストール)
duduclaw expert install ./my-first-pack

# 何がインストールされたか確認
duduclaw expert list

# 共有可能なzipにパッケージング
duduclaw expert pack ./my-first-pack

# 相手がインストール(ローカルzipでもURLでも可)
duduclaw expert install ./my-first-pack-0.1.0.zip
duduclaw expert install https://example.com/my-first-pack-0.1.0.zip

# きれいに削除(パックの社員、同梱スキル、wikiページを削除)
duduclaw expert remove my-first-pack
```

インストール側の防御は組み込み済みです:zip-slipフェンス、50MB上限、コンテンツスキャン。**フックは常に隔離ディレクトリ(`hooks-disabled/`)に無効化された状態でインストール**され、オペレーターが明示的に信頼を許可するまで有効化されません。パックを書くときは、フックが自動的に有効になると想定しないでください。

## 3. 発展編:チーム、wikiページ、要件宣言

- **マルチエージェントチーム**:複数の`[[expert.agents]]`エントリ、`reports_to`で階層を組む(インストール時にトポロジカル順で自動作成)、`department`で部門分け。
- **SOP/ナレッジ**:`wiki/<namespace>/*.md`は共有ナレッジベースにインストールされます。法規、トークスクリプト、価格表はここに置き、SOULには詰め込まないでください。
- **要件宣言**:`[expert.requires]`の`env`(必要な環境変数)と`bins`(必要な外部コマンド)により、インストールする側はインストール前に前提条件を把握できます——インストール後に問題に気づくことがなくなります。
- **推奨プロンプト**:`[expert.prompts] recommended`に3〜5行の「まずはこれを試す」を並べます。インストールした人のファーストウィンになります。

## 4. 既存資産からの変換

- 既存のDuDuClawチームをお持ちですか?`duduclaw expert convert-teams`でチームプレイブックを一括でパックに変換できます。
- Claude Codeエコシステムに公開したいですか?`duduclaw expert export <slug> --format claude-plugin`でプラグイン形式に変換できます。

## 5. 公開と品質

現在の公開方法:zipをダウンロード可能な任意のURLに置く(GitHub Releaseが最も手軽です)。相手は`expert install <url>`でインストールします。テンプレートギャラリー(`distribution/gallery/`)にページを追加するのも歓迎です。集中型レジストリ(PR提出+自動検証+署名)は構築中です。

品質の目安(将来の等級付きスコアカードはこれらを見ます):
- [ ] SOULに明確な「境界」セクションがある
- [ ] `requires`が前提条件を正直に列挙している
- [ ] evalケースを添付している(`duduclaw eval-scaffold`でSOULから草案を作成可能)——evalのあるパックは評価が一段高くなる
- [ ] CHANGELOG形式のバージョン説明(どのバージョンで何が変わったか)がある
