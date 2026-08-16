# 録画 → スキル（Recording to Skill）

人間による実演（ブラウザ操作またはデスクトップ操作）を一度録画し、再生可能な
SKILL.md ドラフトに蒸留します。管理者の承認を経て、正式なスキルとしてインストール
されます。「AI 従業員にレポート照会 / フォーム入力 SOP を教える」といった場面に
向いています。

## 前提条件

1. **機能の有効化（デフォルトは無効）**：対象 agent の `agent.toml` に以下を追加します。

   ```toml
   [capabilities]
   recording = true
   ```

   このスイッチが有効でない場合、5 つの録画ツールはすべて MCP dispatch の入口で
   拒否されます（fail-closed）。
2. **MCP scope**：外部キーには `recording` scope が必要です（組み込み agent の
   デフォルト principal にはすでに Admin が含まれています）。
3. **ブラウザ録画**には Node.js と playwright モジュールが必要です。

   ```bash
   npm install -g playwright
   npx playwright install chromium
   ```

   モジュールが見つからない場合は環境変数で指定できます：
   `DUDUCLAW_PLAYWRIGHT_NODE_PATH=/path/to/node_modules`、
   `DUDUCLAW_NODE=/path/to/node`。
4. **デスクトップ録画**は現時点で macOS のみ対応しており、「画面収録」権限の付与が
   必要です（システム設定 → プライバシーとセキュリティ → 画面収録）。

## ツール一覧

| ツール | 用途 |
|------|------|
| `browser_record_start(url, name?, headless?, max_seconds?)` | tracing + HAR 付きの実際のブラウザを開き、人間が操作を実演する |
| `browser_record_stop(id)` | 録画を停止し、`trace.zip` / `session.har`（自動でマスク処理済み）/ `actions.json` を書き出す |
| `desktop_record_start(name?, max_seconds?)` | デスクトップ録画：1 秒ごとのスクリーンショット＋前面ウィンドウのタイトル（キー入力内容は記録しない） |
| `desktop_record_stop(id)` | デスクトップ録画を停止する |
| `skill_from_recording(id, name?)` | 録画を SKILL.md ドラフトに蒸留し、審査に提出する |

録画ファイルは `~/.duduclaw/recordings/<id>/`（ディレクトリ権限 700）に保存され、
30 分（調整可能、上限 2 時間）で自動停止する安全上限が設けられています。

## 典型的な流れ

1. agent に「レポート照会を一度実演するから録画して」と伝えると、agent は
   `browser_record_start(url="https://erp.example.com", name="月次レポート SOP")`
   を呼び出します。
2. 人間が開かれたブラウザウィンドウで一連の操作を完了させ、ウィンドウを閉じるか
   agent に `browser_record_stop(id)` の呼び出しを依頼します。
3. agent が `skill_from_recording(id)` を呼び出します。
   - マスク処理済みの HAR（静的リソース以外の API 呼び出しの method / URL / body
     の骨格）と UI 操作のシーケンスを解析し、LLM に渡して SKILL.md に蒸留します
     （frontmatter には `name` / `trigger` / `skill_type` / `requires_env` を含む）。
   - まず決定的なセキュリティスキャン（prompt-injection ルールを含む）を通過させ、
     High/Critical のリスクが検出された場合は直接ブロックします。
   - 通過後は隔離されたドラフト領域 `~/.duduclaw/skills-drafts/<id>/SKILL.md` に
     書き込まれ、承認リクエストが作成されます。
4. 管理者が dashboard の承認センターで承認すると、スキルが自動でインストールされ
   有効になります。却下された場合はドラフト領域に残ります。

## セキュリティ設計

- **バックグラウンドでの無断録画はしない**：開始・終了それぞれで明確な返信と
  ログシグナルを出します。
- **HAR のマスク処理**：`Authorization` / `Cookie` / `Set-Cookie` などの header 値、
  すべての cookie 値、token らしき query パラメータや JSON body フィールドは、
  すべて `<env:VAR>` プレースホルダーに置き換えられます。蒸留された SKILL.md は
  必要な環境変数を `requires_env` に列挙するだけで、実際の認証情報は一切含みません。
- **デスクトップ録画はキー入力内容に触れない**：「どのウィンドウに切り替えたか」
  のみを記録します。入力イベントストリーム（rdev）は未実装です。
- **スキルライブラリへ直接は入らない**：蒸留された成果物は必ず自前のスキル承認
  パイプライン（ドラフト隔離領域 → 人手による承認 → インストール）を経由し、
  インストール時にもう一度セキュリティスキャンが実行されます。

## 既知の制限

- デスクトップ録画は現状「スクリーンショット＋前面ウィンドウ」の縮退版です。
  入力イベントストリームはなく、蒸留はウィンドウ切り替えの順序のみに基づきます。
- デスクトップの再生は本質的に computer use タスク（`skill_type: desktop-sop`）
  であり、再生時は 1 ステップずつ実行し、各ステップをスクリーンショットで検証、
  失敗すれば即座に停止します。
- ブラウザ録画にはローカルで利用可能な Playwright が必要です。`headless=true` は
  検証用途にのみ適しています（人間による実演にはデフォルトの有頭モードを使って
  ください）。
