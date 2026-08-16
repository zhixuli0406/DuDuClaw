# フィードバック・提案ページ(GitHub Pages + Haiku自動整理)

エンドユーザーがGitHubを知らなくても問題を報告できるようにする仕組みです:中国語(繁体字)のフォームに入力すると、内容が自動的にGitHub issueとして整理され、Haikuが分類・ラベル付け・フォーマットを担当します。自前のサーバーは一切不要です。

- **フォームURL**:<https://zhixuli0406.github.io/DuDuClaw/>
- **報告の送り先**:本リポジトリの[Issues](https://github.com/zhixuli0406/DuDuClaw/issues)(`feedback`ラベル)

## 動作の流れ

```
ユーザーがフォームに入力(GitHub Pagesの静的ページ、シークレットなし)
   │  Markdownを組み立て、事前入力済みのGitHub issueページへ誘導
   ▼
ユーザーがGitHub上で送信ボタンを押す(スクリーンショット/動画をドラッグ可能、GitHubネイティブのアップロード)
   │  issue本文に<!-- duduclaw-feedback-form v1 -->マーカーが付く
   ▼
GitHub Actions(feedback-triage.yml、マーカー付きissueのみ処理)
   │  claude-haiku-4-5 + structured outputs:分類/重大度/タイトル/フォーマット整形
   ▼
issueを自動書き換え:整理後の内容 + 原文を<details>に格納 + ラベル付け(feedback + カテゴリ)
```

## 関連ファイル

| ファイル | 役割 |
| --- | --- |
| `feedback/index.html` | フォームページ本体(自己完結型HTML、外部依存なし。スタイルはMDSデザインシステムの手書き版) |
| `feedback/inter-latin-wght-normal.woff2` | Inter Variableフォント(Latinサブセット、CDNを使わず同梱) |
| `.github/workflows/deploy-feedback-page.yml` | `feedback/**`変更時にGitHub Pagesへデプロイ |
| `.github/workflows/feedback-triage.yml` | issue作成時にHaiku整理をトリガー |

## 初期設定(一度だけ)

1. GitHub Pagesがworkflowモードに設定済みであること(`gh api -X POST repos/<owner>/DuDuClaw/pages -f build_type=workflow`)。
2. リポジトリシークレット`ANTHROPIC_API_KEY`:`gh secret set ANTHROPIC_API_KEY`。未設定の場合、triageは単にスキップされ(issueはそのまま残る)、フォーム自体のフローには影響しません。
3. `feedback`ラベル(作成済み。削除するとtriageのラベル付けが失敗します)。

## セキュリティ設計

- **フロントエンドはゼロシークレット**:APIキーとトークンはActions secretsにのみ存在し、静的ページからは取得できません。
- **プロンプトインジェクション対策**:issue内容はXMLタグで囲まれ明示的にデータへ降格されます。モデル出力はJSON schemaで制約され(分類は4つのenum値のいずれかにしか落ちません)、原文は常に`<details>`に保持され、整理に失敗した場合issueはそのままです。
- **スクリプトインジェクション対策**:workflowはissue本文をシェルに埋め込まず、`gh api`でファイルへ取得し`jq`でJSONを組み立てます。
- **コスト**:フォームマーカー付きのissueのみがトリガーされ、入力は16k文字で切り詰められ、Haiku1回あたりのコストは約$0.01以下です。

## フォームの改修

`feedback/index.html`を変更してmainにpushすれば自動的に再デプロイされます。フィールドを変更した際は、`feedback-triage.yml`内のsystem promptのセクション名(問題の説明(問題描述)/再現手順(重現步驟)/期待される動作(預期行為)/環境(環境))も忘れずに同期してください。

スタイルはMDSデザインシステムに準拠します(ダッシュボードの`web/src/components/mds/`と同一系統):OKLCHカラートークン、レイヤー化されたsurface、radius体系(ボタン/入力欄10px、カード14px)、Inter+繁体字システムフォントのフォールバック、フォントウェイトは400/500のみ、brand blueのCTA、3pxのfocus ring。ビルド不要の静的ページであるため、トークンは`<style>`内にCSSカスタムプロパティとして直接手書きされており、MDSトークンの変更時は手動同期が必要です。ダークモードは`prefers-color-scheme`を使用します(ダッシュボードの`.dark`クラス機構は静的ページには適用されません)。
