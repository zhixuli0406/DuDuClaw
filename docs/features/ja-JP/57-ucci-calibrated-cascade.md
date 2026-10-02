# UCCI キャリブレーション付きカスケード

DuDuClaw は、Varun Kotte 氏の [UCCI: Calibrated Uncertainty for Cost-Optimal
LLM Cascade Routing](https://arxiv.org/abs/2605.18796)（[参考実装](https://github.com/varunkotte6/ucci)）が出力する router ファイルを使い、生成後にエスカレーションするかどうかを判断できます。Rust の依存関係は特定の commit に固定されており、router ファイルは UCCI のバージョン付き JSON 形式を使います。判断に使う信号は UCCI の平均 top-2 token margin 不確実性であり、DuDuClaw が以前使っていた平均 logprob スコアではありません。

## 収集とフィッティング

`inference.toml` で既存の router と observation ファイルを有効にします。

```toml
[router]
enabled = true
fast_model = "your-fast-model"
strong_model = "your-strong-model"
ucci_observations = "ucci/observations.jsonl"
ucci_drop_stop_token = true # for vLLM/llama.cpp if logprobs include EOS
ucci_shadow_strong = true    # opt-in: collect Strong answers for returned Fast replies
ucci_shadow_max_inflight = 1 # cap on concurrent background shadow generations (default 1)
```

必要な top-2 logprobs を提供できるのは、現時点では OpenAI-compatible 推論バックエンドだけです。収集時は temperature を 0 に固定します。サーバーが内容トークンごとに 2 つの候補を返さない場合、`u` は null となり、キャリブレーション済みの判断は行われません。

2026-09-29 以降、このバックエンドは独自の HTTP client を持ちません。`logprobs` / `top_logprobs` のリクエストフィールドとトークンごとのレスポンス解析は、共有の `duduclaw-llm` OpenAI-compat provider に移りました。ローカルバックエンドはその上の薄いラッパーで、ローカル固有の 2 つの設定（300 秒のリクエストタイムアウトと、サーバー上の `qwen/qwen3-4b` が `provider/model` 形式の修飾子と誤認されないよう model id をそのまま送ること）だけを保持します。UCCI margin の計算は意図的に inference 側に残しています。共有 provider は信号を運ぶだけでスコアリングはしないため、`ucci_fit.py` / `ucci_pair.py` の入力形式は変わりません。observation ファイルにはプロンプトと回答が平文で含まれるため、opt-in です。各行には、カスケード共通の `request_id`、一意な `id`、`stage`、`u`、回答、モデル、レイテンシ、空の人手ラベル欄があります。

Fast → Strong では、2 つ目の回答を `ucci_shadow_strong` が提供します。Strong の生成は**バックグラウンド**で実行されます。Fast の応答は受理され次第返され、2 回目のモデル呼び出しを待ちません。そのため、収集を有効にして下がるのはホストのスループットであり、ユーザーが感じるレイテンシではありません。結果として、observation の行がそれが説明する応答の**後**に追記されることがあり、並行リクエストの行が入り混じることもあります。行のペアリングは必ず `request_id` で行い、ファイル順には頼らないでください（`scripts/ucci_pair.py` は既にそうしています）。各追記はプロセス間 advisory lock を保持するため、行が途中で切れたり行内で混ざったりすることはありません。shadow の生成中にプロセスが終了すると、失われるのはその最後の 1 行だけです。待機したい組み込み側は `InferenceEngine::flush_shadow_observations()` で実行中のものを待てます。

shadow をバックグラウンドタスクに切り離したため、次のリクエストのフォアグラウンド生成と重なり、同じバックエンド／モデルスロットを取り合う可能性があります。`ucci_shadow_max_inflight`（デフォルト `1`）は同時に走る shadow 生成の数を制限します。上限に達している状態で新しい shadow を始めようとすると**スキップ**されます。キューには入らず、フォアグラウンドの応答もブロックせず、スキップ回数はカウントされて `debug!` ログに出力されます。バックエンドが重なった生成を実際に処理できる場合（例: バッチ処理に対応したサーバー）に限って増やしてください。`0` は `1` と同じ扱いで、無制限にはなりません。

手動レビュー用のテンプレートは次のように作成します。

```sh
python3 scripts/ucci_pair.py --observations observations.jsonl \
  --stage local_fast --out fast-review.jsonl
```

Strong → Cloud では、同じ `request_id` と `stage = "cloud_api"` で Cloud の回答を別途収集し、同じ helper に `--cloud-observations cloud.jsonl --stage local_strong` を渡します。**両方**の回答を手動でレビューし、ラベル欄を埋めてください。

```json
{"id":"example-local-fast","stage":"local_fast","u":0.42,"answer":"local answer","large_answer":"strong answer","small_correct":0,"large_correct":1,"label_source":"human","split":"cal"}
```

Fast → Strong には `stage = "local_fast"`、Strong → Cloud には `stage = "local_strong"` を使います。router の `escalated` の判断や LLM judge の判定を、正解率のラベルとして使わないでください。後者は監査用に別途保存できます。現在のゲートがエスカレーションしたかどうかに関係なく例を含めてください。エスカレーションされた例だけを選ぶとフィットに偏りが生じます。キャリブレーション、検証、テストの例は互いに重ならないようにします。helper はローカルの応答として `answer` と `small_answer` のどちらも受け付けます。

`ucci-router` をインストールし、各ステージを**選んだ正解率ターゲット**と**実測した 1 回あたりのコスト**でフィットします。

```sh
python3 -m pip install ucci-router==0.1.1
python3 scripts/ucci_fit.py --data reviewed.jsonl --stage local_fast \
  --tau 0.90 --c-small 1 --c-large 3 --out fast-router.json
python3 scripts/ucci_fit.py --data reviewed.jsonl --stage local_strong \
  --tau 0.95 --c-small 3 --c-large 12 --out strong-router.json
```

上の値はコマンドの形を示すための例です。ターゲットとコストは自分のワークロードに合わせて選んでください。helper は人手ではないラベルと、回答のペアがない行を拒否します。UCCI の `fit` は `--cost-model sequential` で呼び出されます。DuDuClaw は現在のモデルを実行してから、次のモデルに支払うかどうかを決めるためです。UCCI は、isotonic map のフィットに独立したキャリブレーション分割を、しきい値の選択に検証分割を、最終評価にテスト分割を使います。フィット済みファイルを有効にする前に、`ucci evaluate --router fast-router.json --data fast-router.json.reviewed.jsonl --split test` と、対応する `ucci report` コマンドを実行してください。helper は各 router の隣にラベルだけの入力を保存するため、UCCI は分割を再現してデータダイジェストを検証できます。各ファイルとあわせて、レビュー済みの元の回答とモデルバージョンも保管してください。

## 運用

フィット済みファイルを検証したら、次のように設定します。

```toml
[router]
enabled = true
ucci_fast_router = "ucci/fast-router.json"
ucci_strong_router = "ucci/strong-router.json"
ucci_observations = "ucci/observations.jsonl"
ucci_drop_stop_token = true
local_tools = false # for a dedicated bare-completion calibration trial
```

相対パスは DuDuClaw のホームディレクトリから解決されます。router が読み込まれるのは、ファイルが有効で、cost model が `sequential` の場合だけです。各ローカル階層は、それぞれフィットした router を使います。UCCI は、キャリブレーション済みの誤り確率がその階層のしきい値を**厳密に上回る**場合にエスカレーションします。**設定済み**の階層では、ファイルがない、top-2 信号がない、バックエンドが未対応のいずれの場合もエスカレーションします。UCCI ファイルのない階層は、ローカルの回答を受け入れます。キャリブレーション済みゲートが有効だとみなす前に、警告と observation の行を確認してください。UCCI は、router における唯一のキャリブレーションゲートになりました。従来の `post_hoc_enabled` / `post_hoc_alpha` / `post_hoc_beta` / `post_hoc_accept_threshold` ロジスティック設定は 2026-09-29 に削除されました（`wiki/reports/feature-audit-2026-09-29.md` T3-S7）。デフォルト値が一度もフィットされておらず、alpha 4.0 / beta -2.0 / threshold 0.5 では「確率」は平均 logprob >= ln 0.5 という固定の閾値にすぎず、スコアと結果ラベルを並べて保存する処理も存在しなかったためです。この 4 つのキーが `inference.toml` に残っていても無視されます。UCCI ファイルのない階層は、ローカルの回答をそのまま受け入れます。

## 現在の制約

gateway の MCP tool loop は別の provider 経路を使い、`InferenceEngine::route_and_generate` を通りません。その応答はこのキャリブレーション済みゲートの対象外です。フィット済みゲートを評価するには、`local_tools = false` の専用のベア補完（bare-completion）ワークロードを使ってください。ツールが必要なタスクは、既存のツール対応経路を使います。`ucci_shadow_strong` は、そのまま返されるはずの Fast 応答について Strong の回答を記録します。これはバックグラウンドで行われ、応答が届いた後に書き込まれることもあります（上記参照）。observation ファイルは Cloud の shadow 回答を実行せず、結果ラベルも付与しません。Strong → Cloud の検証には、Cloud の出力を別途収集してください。2 段のカスケードにはステージ固有のデータも必要です。すべての Strong リクエストで学習した Strong → Cloud のフィットは、Fast ゲートを通過して Strong に到達した母集団とは異なる可能性があります。
