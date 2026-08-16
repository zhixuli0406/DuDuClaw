# ADR-005: ドキュメントエクスポート(md → Slide / Word / PPT / PDF)

- Status: Accepted
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

顧客はデモの中で(7:35)、agent が Google Slides や Microsoft Office ファイルを
生成できるかを尋ねた。先方自身も、この領域は複雑でまだ調査中であると認めている。

現状のアンカー:**現時点では何もない。** Rust ソース全体を grep しても、
pptx / docx / pandoc / `docx-rs` / `rust_xlsxwriter` の実装は見つからない。
`pptx` / `docx` という文字列がヒットする唯一の場所はビルド後の dashboard
バンドル(`PartnerPortalPage`)内であり、これはパートナー向け素材の
ダウンロードリンクであって、ドキュメント生成とは無関係である。ReportPage の
export はフロントエンドの JS 関数であり、送付可能なファイルは生成しない。

agent の出力は本質的に markdown である。これを顧客が受け取り、開き、編集できる
Slide / Word / PPT / PDF に変換するには、変換パイプラインが必要になる。これは
技術選定のスパイクである——まず方向性を定め、それから実装に入る
(WP11-T11.2 の `document_export` MCP ツールに対応)。

## Options considered

**(a) 純粋な Rust: `docx-rs` / `rust_xlsxwriter`**
外部依存ゼロ、シングルバイナリへのパッケージングがクリーン、プラットフォーム間で
一貫している。欠点は pptx のエコシステムが弱いこと——Rust には成熟した pptx
生成ライブラリが存在せず、OOXML を自前で組み立てるコストが高い。docx は
実現可能だが、pptx が致命的な弱点となる。

**(b) Pandoc サブプロセス**
md → docx は極めて成熟しており、md → pptx も使用可能である(Pandoc には
pptx writer がある)。欠点は外部バイナリ依存が必要になることで、ユーザーの
マシンに Pandoc が入っているとは限らない。これは detect-then-enable で
解消できる:Pandoc を検出した場合のみ有効化し、検出できなければ fail-soft で
md 添付ファイルへとデグレードする。

**(c) HTML → PDF(ヘッドレスブラウザ)**
プロジェクトにはすでにブラウザ層(L3)として Playwright MCP があり、理論上は
md → HTML → PDF への印刷が可能である。PDF の品質は高く、レイアウトも
制御可能なはずである。**現状に関する正直な警告**:gateway の
`browser_router.rs` は現時点ではスケルトンにすぎず、実際のブラウザ自動化は
この router ではなく Playwright MCP 経由で動いており、PDF 出力の完全な
ループはまだ配線されていない。「すでにあるもの」として扱うことはできない。

**(d) Google Slides API**
ネイティブな Google Slides であり、Google Workspace をヘビーに利用する顧客に
最も馴染む。欠点は OAuth が必要で、データがクラウドを経由し、実装と認可の
維持コストが高いことである。オンプレミス優先という製品の方向性と衝突する。

## Decision

**md → docx / pptx は Pandoc 経由とする(detect-then-enable、fail-soft で
md 添付ファイルへデグレード)。PDF は既存のブラウザ層を経由する。純粋な
Rust 経路は今後の選択肢として保持する。**

理由:Pandoc は最も要求されることの多い 2 つのフォーマット、docx と pptx を
一度に押さえられ、pptx はまさに純粋な Rust が苦手とする部分である。
detect-then-enable は「外部依存が必要」という欠点を、優雅な撤退へと格下げする
——Pandoc がなければ md 添付ファイルに一言の説明を添えて戻すだけで、クラッシュ
せず、成功したふりもしない。PDF をブラウザ層に乗せるのが最短経路であり、
新たな依存を追加しない。Google Slides のネイティブ対応は OAuth とクラウドへの
データフローを伴い、オンプレミス優先の製品方向性と衝突するため、今回は
行わない。

実装のポイント(詳細は WP11-T11.2):
- MCP ツール `document_export`:md コンテンツ + 目標フォーマット
  (docx / pptx / pdf)を入力とし、生成ファイルを agent workspace に置き、
  チャネルからファイルメッセージとして送出する。
- pptx の最小テンプレート:タイトルページ + bullet ページ、DuDuClaw の
  ブランドカラーを適用。
- Pandoc が存在しない場合 → fail-soft で md 添付ファイルへデグレード
  (最も保守的な利用可能な出力への fail-open であり、静かな失敗ではない)。

## Consequences

**得られるもの:** md → Office の 2 大フォーマット(docx / pptx)に明確で
成熟した経路がある;Pandoc のない環境でも壊れず、md を受け取れるだけである;
OAuth やクラウドデータフローを持ち込まない。

**支払うもの:** Pandoc は実行時の外部依存であり、デプロイ文書にインストール
方法を明記する必要がある——インストールして初めて Office 形式の出力が得られる。
PDF が依存するブラウザ層は現時点ではスケルトンであり、PDF についてはブラウザの
ループが実際に配線されるまでは実用にならない——この点を顧客に対して
曖昧にしてはならない。

**顧客への正直なトークポイント:** 「md → Office はサポート済みの方向性
(docx / pptx)、Google Slides のネイティブ対応は現在も評価中」。Google Slides
を約束せず、PDF の現状を誇張しない。

**今後の選択肢:** 外部依存が実際の痛点となった場合(例:顧客がシングルバイナリを
求め、Pandoc のインストールを禁止する場合)、純粋な Rust の `docx-rs`
経路を有効化する。その時点で、pptx のために OOXML を自前実装する価値が
あるかを別途評価する。その転換は新しい ADR で記録する。
