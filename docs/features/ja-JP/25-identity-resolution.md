# アイデンティティ解決

> 「この人は誰か」の唯一の信頼できる情報源——1つのprovider trait、3つのバックエンド、そしてAgentが毎ターン読む`<sender>`ブロック。

---

## たとえ話：受付係とディレクトリ

正面玄関に受付係がいるビルを想像してください。来訪者が入ってくると、受付係は推測しません。会社のディレクトリ——「これはRuby Lin、顧客PM、AlphaとBetaの2プロジェクトに許可済み」と教えてくれる権威ある仕組み——を確認します。

しかしディレクトリサーバーは時折メンテナンスで停止します。優れた受付係は引き出しに印刷した名簿——ディレクトリに最後に接続できたときのキャッシュコピー——を入れています。オンラインの仕組みがオフラインのとき、彼らは全員を玄関で追い返すのではなく、その紙に切り替えます。

そして一度受付係があなたを特定したら、すべての部署に再検証させたりはしません。あなたの胸に **来訪者バッジ** を留め、そこには氏名、役割、入れる階が書かれています。あなたが訪れる各部署は、ディレクトリを再確認するのではなくバッジを読みます。

DuDuClawのアイデンティティ解決はまさにこれです：

- **ディレクトリ** は上流provider（`NotionIdentityProvider`）。
- **引き出しの印刷名簿** はwikiキャッシュ（`WikiCacheIdentityProvider`）。
- **受付係のフォールバックロジック** は`ChainedProvider`（ライブ → キャッシュ）。
- **来訪者バッジ** はAgentのシステムプロンプトに注入される`<sender>`ブロック——一度解決し、毎ターン読まれる。

---

## 解決される問題

RFC-21 §1以前、DuDuClawのAgentには「私に話しかけているこの人は誰か？」を尋ねる方法がありませんでした。利用できる唯一の仕組みは、`identity/discord-users.md`のような手で知っているパスに`shared_wiki_read`を呼ぶことでした。そのファイルには2人しか載っていませんでした。それ以外の全員——チームメンバー、顧客の連絡先、エンジニア——は見えない見知らぬ人でした。

AgentはSOUL.mdで「非プロジェクトメンバーを拒否する」のようなルールを宣言しましたが、そのルールを評価する名簿も、権威ある情報源に問い合わせる仕組みもありませんでした。境界は散文の中に存在し、データの中にはありませんでした。

修正の方法は **システム層の権威であって、プロンプト層の提案ではない**：`IdentityProvider` traitを導入し、wikiを信頼できる情報源から透過的なキャッシュへ降格させ、解決された人物を構造化データとしてシステムプロンプトに供給する——SOUL.mdルールを願望ではなく評価可能にするのです。

---

## Provider Trait

`IdentityProvider`（`duduclaw-identity` crate内）は小さなasync traitです。本番のprovider（Notion、LDAP、カスタムHTTP）はネットワークIOを伴うため、すべてのメソッドはasyncですが、純粋にローカルな実装も同じ面に準拠するため、呼び出し箇所を変えずに差し替えできます。

```
#[async_trait]
trait IdentityProvider: Send + Sync {
    async fn resolve_by_channel(channel, external_id)
        -> Result<Option<ResolvedPerson>, IdentityError>;

    async fn lookup_project_members(project_id)
        -> Result<Vec<ResolvedPerson>, IdentityError>;

    fn name(&self) -> &str;   // "notion" / "wiki-cache" / "chained"
}
```

### `Ok(None)`の意味

重要な設計判断：人物が不明のとき、`resolve_by_channel`は`Ok(None)`を返します。これは通常の「見知らぬ人がメッセージを送る」ケースであり、明示的に **エラーではありません**。`Err`は本物のproviderの失敗——到達不能な上流、不正なペイロード、IO障害——のために予約されています。この区別こそが、チェーン化したproviderの優雅な縮退を可能にします。

---

## 3つのProvider

| Provider | 情報源 | 振る舞い |
|----------|--------|----------|
| `WikiCacheIdentityProvider` | `<home>/shared/wiki/identity/people/*.md` | ローカルMarkdownからYAML frontmatterレコードを読む。不正な単一ファイルはスキップされ（`tracing::warn!`付き）、解決器全体を落とすことはない。 |
| `NotionIdentityProvider` | Notion `databases/query` API | 1行1人のPeople DBを問い合わせ；operatorが設定可能な`field_map`で論理フィールドをNotionプロパティ名にマッピング。5xx/ネットワーク → `Unreachable`；4xx/schema → `Malformed`。 |
| `ChainedProvider` | キャッシュ → 上流 | まずキャッシュを試す；ミスなら上流へ落ちる；上流障害時はエラーではなく「解決不能」へ縮退。 |

### WikiCacheIdentityProvider schema

`identity/people/`配下の各`*.md`ファイルはYAML frontmatterブロックを持ち、本文はproviderが無視する自由形式のメモです：

```
---
person_id: person_2f9
display_name: Ruby Lin
roles: [customer-pm]
project_ids: [proj-alpha, proj-beta]
emails: [ruby@example.com]
channel_handles:
  discord: "1234567890"
  line: "Uabc"
---

Rubyについての自由形式のメモ——providerは決してここを読まない。
```

### NotionIdentityProvider フィールドマップ

Notionのプロパティ名はデプロイごとに異なるため、operatorは`NotionFieldMap`を宣言します。デフォルトは妥当な慣例に合いますが、すべてのフィールドは上書き可能です：

```
field_map = {
  name     = "Name",
  roles    = "Roles",
  projects = "Projects",
  channel_props = {
    discord  = "Discord ID",
    line     = "Line ID",
    telegram = "Telegram ID",
    email    = "Email",
  },
}
```

各`resolve_by_channel`呼び出しは、channel-handleプロパティが`external_id`に等しいレコードに絞るfilterで`databases/query`を問い合わせます。

---

## ChainedProvider フォールバック機構

`ChainedProvider`は受付係のフォールバックの頭脳です。高速なキャッシュと低速な権威ある上流を包みます：

```
resolve_by_channel(channel, external_id)
        |
        v
  ┌─────────────────────────────┐
  │ 1. キャッシュ高速パス        │
  │    cache.resolve(...)       │
  └─────────────────────────────┘
        |
   Ok(Some) ──────────────► キャッシュ内の人物を返す（短絡）
        |
   Ok(None)〔ミス〕          Err〔キャッシュ障害〕
        |                        |
        |   warn! 「キャッシュエラー——上流へ落ちる」
        v                        v
  ┌─────────────────────────────┐
  │ 2. 上流低速パス             │
  │    upstream.resolve(...)    │
  └─────────────────────────────┘
        |
   Ok(person) ────────────► 上流の結果を返す
        |
   Err〔上流障害〕
        |
   warn! 「上流エラー——解決不能へ縮退」
        v
   Ok(None) を返す   ← Agentは送信者を見知らぬ人として扱う、ハードエラーではない
```

重要な特性：Notionが到達不能でも、チャンネル返信は進行し続けます。Agentは`<sender>`を見ないだけで、メッセージを見知らぬ人からのものとして扱います——まさに受付係が印刷名簿に退き、決して玄関を閉ざさないのと同じです。

`lookup_project_members`は好みを反転させます：プロジェクトメンバーシップはまさにキャッシュでドリフトしやすいデータなので **まず上流を問い合わせます**。上流がエラーになったときだけキャッシュに退きます（そして`tracing::warn!`を出してoperatorがこの縮退に気づくようにします）。

---

## ResolvedPerson レコード

解決の成功は正規化された`ResolvedPerson`を返します。これらを生成できるのは上流providerだけ；下流の呼び出し者は不変の検索結果として受け取ります。

| フィールド | 型 | 意味 |
|-----------|----|----|
| `person_id` | `String` | 信頼できる情報源からの安定した正規id（例：Notion page id）。不透明として扱う。 |
| `display_name` | `String` | 人間が読める名前、例：「Ruby Lin」。 |
| `roles` | `Vec<String>` | ドメインの役割、例：`["customer-pm", "engineer"]`。 |
| `project_ids` | `Vec<String>` | プロジェクトメンバーシップ——「非プロジェクトメンバーを拒否する」が評価する対象。 |
| `emails` | `Vec<String>` | 関連するメールアドレス；空の場合もある。 |
| `channel_handles` | `BTreeMap<String, String>` | `{channel-wire-name: external_id}`。シリアライズ順序を確定させるため`BTreeMap`。 |
| `source` | `String` | このレコードを生成したprovider（`"notion"`、`"wiki-cache"`）。監査ログに反映される。 |
| `fetched_at` | `DateTime<Utc>` | キャッシュレコードはキャッシュ書き込み時刻；ライブレコードは上流取得時刻を持つ。 |

`ChannelKind` enumはDiscord、Line、Telegram、Slack、Whatsapp、Feishu、Webchat、Emailを網羅し、さらに`Other(String)`の万能項を持つため、セルフホストのwebhookや将来のチャンネルが解析に失敗することはありません。

---

## identity_resolve MCP ツール

Agentはアイデンティティ解決に1つのMCPツールでアクセスし、専用scopeで管理されます：

```
identity_resolve { channel, external_id }
        |
        v
  Scopeチェック：呼び出し元principalはScope::IdentityReadを保持する必要がある
        |  （scope欠如 → 拒否、fail closed）
        v
  provider.resolve_by_channel(channel, external_id)
        |
        v
  ResolvedPerson JSON   ── または ──   null（不明な送信者）
```

このscopeゲートはDuDuClawの「セキュリティゲートはfail closed」の慣例に従います——`Scope::IdentityRead`を持たないキーは拒否され、決して黙って通されません。

---

## `<sender>`ブロック：データとしてのアイデンティティ

最も影響力のある統合は自動的です。チャンネルメッセージが到着すると、gatewayは送信者を **ターンごとに一度** 解決し、XMLで区切られた`<sender>`ブロックをシステムプロンプトに注入します：

```
システムプロンプト
  ├─ SOUL.md（人格 + ルール）
  ├─ ## Your Team（サブAgent名簿）
  ├─ <sender>                          ← 注入、ターンごとに一度解決
  │    <person_id>person_2f9</person_id>
  │    <name>Ruby Lin</name>
  │    <roles>customer-pm</roles>
  │    <project_ids>proj-alpha, proj-beta</project_ids>
  │    <source>notion</source>
  │  </sender>
  └─ ... 残りのコンテキスト
```

これが来訪者バッジです。この機能以前は、「非プロジェクトメンバーを拒否する」のようなSOUL.mdルールは、Agentが推論の途中で`shared_wiki_read`を呼ぶことを覚えている必要がありました——その手順をしばしば飛ばしていました。今やメンバーシップデータはすでにAgentの目の前、高注意のスロットに、毎ターン存在します。ルールはAgentがすでに保持しているデータから評価可能になります。

providerが未設定、または送信者が不明のとき、`<sender>`ブロックは注入されません——Agentはメッセージを見知らぬ人からのものとして扱い、SOUL.mdの見知らぬ人の処理ルールが適用されます。

---

## なぜ重要か

### プロンプト層の希望ではなく、システム層の権威

SOUL.md命令はベストエフォートです——モデルは従うかもしれないし従わないかもしれません。アイデンティティ解決は境界を、実データに対して評価できる場所へ移します。「非プロジェクトメンバーを拒否する」はプロンプトであることをやめ、Agentが実際に見られる`project_ids`に対するチェックになります。

### 優雅な縮退、決して閉ざされない扉

`ChainedProvider`のソフトフェイル設計は、上流障害が会話を壊すのではなく忠実度を下げる（不明な送信者）ことを意味します。Notionのメンテナンス時間帯はあなたのAgentを倒しません——wikiキャッシュへ、さらに見知らぬ人の処理へ退き、返信を続けます。

### wikiは情報源ではなくキャッシュになる

`WikiCacheIdentityProvider`を3つのバックエンドの1つにすることで、共有wikiは「Agentが手作りでアイデンティティ検索をする場所」から「権威ある仕組みの透過的なキャッシュ」へ降格します。これは進化ループがwikiを外部データの管理されていないコピーへと密かにドリフトさせるのを防ぎます。

### forkではなくtraitでプラグイン

各バックエンドが同じ`IdentityProvider` traitを実装するため、NotionをLDAPやカスタムHTTPディレクトリに差し替えることはproviderの変更であって、チャンネル返信パスの書き換えではありません。`<sender>`注入、MCPツール、scopeゲートはすべて同一のまま保たれます。

---

## まとめ

来訪者を推測する受付係は負債です。ディレクトリを確認し、ディレクトリ停止時には印刷名簿に退き、各来訪者にバッジを留めてどの部署も再確認せずに済むようにする受付係——それがルールを築ける仕組みです。DuDuClawのアイデンティティ解決は、すべてのAgentにその受付係を与えます：1つのtrait、3つのprovider、優雅な縮退、そしてAgentが毎ターン読む`<sender>`バッジ。「この人は誰か？」は推測であることをやめ、検索になります。
