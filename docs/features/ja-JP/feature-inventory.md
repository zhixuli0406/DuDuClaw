# DuDuClaw 全機能一覧

> v1.24.0 コア + 2026-07/08 追加 | 最終更新：2026-08-16 (v1.61.0)
>
> 注記:以下の各セクションは v1.24.0 のベースラインです。直後の**「追加」**ブロックが、それ以降に実装された機能をまとめています(正式な一覧は `CHANGELOG.md` を参照)。本ファイルは英語版と同期済みです。

---

## 2026-08 追加(v1.54 – v1.61)

| 機能 | 説明 |
|------|------|
| 校正付きフォワードモデル + held-out 学習ゲート(v1.54) | 行動前の確信度予測を proper score(Brier/RPS、log score は不採用)で採点し Murphy 分解——証拠は外部ツール結果であり自己申告ではない;プログラム的証拠のない帰納型教訓は shadow 候補として開始し、サンプル外の Wilson 下界(複数候補は Bonferroni 補正)が凍結ベースラインを上回って初めて昇格;結論は 3 種の誠実ラベルのみ(SUPPORTED / CANDIDATE / INDISTINGUISHABLE_FROM_LUCK)。デフォルト有効、ダッシュボードで層別に無効化可([39-calibrated-forward-model.md](../39-calibrated-forward-model.md)) |
| 通知ガバナンス(v1.55) | すべてのプロアクティブ通知に L1/L2/L3 レベル必須;サイレント時間帯は L1/L2 を延期・統合配信(L3 は常に配信);オプトインの日次ダイジェスト(何もない日は送らない);通知カテゴリ別のアクション率計測(`notify.stats`、精度 <50% は broken 判定);決定カードは決定後その場で畳まれる;チャネル通知にダッシュボード深リンク([40-notification-governance.md](../40-notification-governance.md)) |
| 保留決定パイプラインの統一(v1.55) | 5 種の決定ソース(goal needs_human / 起動承認 / 一般承認 / インストール承認 / 自動ルール停止)を単一の action-id エンコード・単一の認可モデル(従来未認証だった goal ボタンの穴を閉鎖)・統一受信箱に収斂;4 つ目の「引き継ぐ」アクション追加;1 人の決定で全受信者のカードが同期して畳まれる |
| 人間による引き継ぎ(v1.55) | 検証済み管理者がチャネル会話で発言するだけで AI がその会話への返信を停止(デフォルト 60 分、`/takeover` で照会/延長/終了);引き継ぎ中は AI メッセージをその会話へ送るすべての経路を凍結・延期・破棄(L3 承認は配信継続)。個人版の自分との会話が沈黙する問題のため v1.56 でオプトイン化(デフォルト off)([42-human-takeover.md](../42-human-takeover.md)) |
| 常駐センシング(v1.55) | 外部データストリーム(`http_poll` / `command` / `file_tail` / `websocket`、デフォルト off)を `tick` イベントとして autopilot バスへ接続;数値フィールドは `prev_`/`delta_`/`pct_` を自動導出;エージェント起床前の任意のローカルモデルスクリーニング;TickHub インメモリリングバッファ;SSRF/DNS リバインディング防御;実相場フィードでの実地検証により強化([41-resident-sensing.md](../41-resident-sensing.md)) |
| 呼び出し横断の「最近の自身の行動」注入(v1.55) | 各起床の冒頭に、そのエージェント自身の直近 24 時間のツール呼び出し監査ダイジェスト(失敗・ブロックされた呼び出しを含む)を注入——「あれをやった?」への回答は耐久記録が基準になり、ライブツール状態だけに頼らない |
| 5 チャネルの引用返信コンテキスト(v1.55) | Telegram / Discord / Slack / Teams / WhatsApp でメッセージへの返信(引用)時、引用内容がエージェント入力に含まれる;mention-only グループで bot への返信はメンション扱い;Telegram 転送は転送元をラベル付け |
| 学習ルールの平易化(v1.55) | playbook ルールを平易な文として表示(LLM ゼロのテンプレート変換)+「なぜこのルールがあるか」の証拠文;チャネルコマンド `/rules`;注入ルールに番号付与——AI は回答の根拠ルールを引用できる |
| Telegram Mini App 承認カード(v1.55) | 高リスク承認カードにオプトインの「詳細を見る」web-app ボタン——完全な説明、事前シミュレーションの結果、期限カウントダウン、承認/拒否;署名付き `initData` 検証、ボタン押下と同一の認可([43-telegram-miniapp.md](../43-telegram-miniapp.md)) |
| 学習パイプラインの可観測性(v1.56) | `source_facts` を記録したルールはソース事実が置換されると `source-stale` フラグ(注入時に降格 + ラベル);閾値到達後にゲートで阻止された統合失敗を理由付きで記録(`consolidation_failures.jsonl`);会話パスのルール決算も held-out ゲート経由に;帰納型 shadow 候補が会話側でもサンプル外実績を蓄積し、実際に昇格可能に |
| エディションと設定の強化(v1.56) | 個人版の同時実行上限は「同時実行中のゴールタスク数」(デフォルト 2、拒否せずキュー、fail-open;RFC-27)——エージェント数の上限は永遠に設けない;チーム版専用画面はゲートウェイのディスパッチ入口でサーバー側ブロック;`agent.toml [model] account_pool` が実際にローテーション候補を絞る;ダッシュボードのエージェント作成はモデルの明示選択必須 |
| 真の ACP サーバー(v1.57) | `duduclaw acp` が Agent Client Protocol v1(stdio JSON-RPC)を実装——Zed / JetBrains / nvim の agent panel が通信チャネルと同一のゲートウェイ返信パイプラインでエージェントと対話、`tool_call` / `plan` / メッセージチャンクをストリーミング;A2A の `acp-server` コマンドは無変更 |
| Remote MCP + OAuth 2.1(v1.57) | 仕様ネイティブの `POST /mcp` エンドポイント(バージョンネゴシエーション、ステートレスモード、Origin アンカーの許可リスト)+ 最小かつ完全な OAuth 2.1(RFC 9728/8414/7591、PKCE S256、オペレーター同意、refresh ローテーション)——claude.ai カスタムコネクタ / Claude モバイル / MCP Inspector がセルフホストの DuDuClaw に直接接続可能 |
| 5 チャネルのテキスト裁決(v1.57) | 決定カードへの返信でメッセージ全体が裁決語(承認/拒否/再試行/完了/中止/一時停止、中英対応)ならボタン押下と同等——Telegram / Discord / Slack / LINE / Teams、同一の認可・重複押下保護・アクション率計測;スマートウォッチのワンタップ決定の欠落を補完 |
| ローカルモデルマーケットプレイス(v1.57) | 用途を選ぶ → 実機メモリから算出したハードウェア適合ランプ → ワンクリックインストール(検証済み 5 発行者の HF ソースから量子化を自動選択);MoE 二重判定により 16GB マシンでも 30B-A3B 級モデルを合理的に推奨([45-local-model-marketplace.md](../45-local-model-marketplace.md)) |
| ワーキングステート——起床横断の権威状態(v1.57) | エージェントごとのキーバリュー状態 + 引き継ぎノートを全起床(cron / heartbeat / goal loop / チャネル)に唯一の権威として自動注入;更新は明示ツール呼び出しのみ(`reason` 必須 + 置換履歴、`expected_value` CAS、`ttl_hours` による当日ルール失効、32 キー上限);`[memory] working_state_enabled`、デフォルト on([44-working-state.md](../44-working-state.md)) |
| スケジュール実行が記憶と可視性を獲得(v1.57) | 成功した cron/ディスパッチ実行が同じ蒸留/知識パイプラインへ(エージェントごと毎時スロットル)、実行履歴ページにも記録——従来、スケジュール駆動のみのエージェントは何も蓄積せず実行記録もゼロだった |
| エコシステムと配布面(v1.57) | 無料産業スターターパック 6 種(安全境界は無削減)、pack registry のインストール/公開(クライアント側 sha256 + minisign 検証)、CONTRIBUTING.md + パック自作チュートリアル、公開サイトチャットウィジェット(ゲストモード、デフォルト off)+ WordPress プラグイン、Chrome / VS Code 拡張、ウェアラブル書き起こし取り込み(`POST /ingest/transcript`)、`duduclaw tunnel`、LINE 友だち QR/NFC;外部 MCP ツール面は scope 駆動に;Homebrew チャネル廃止 |
| ゴールタスクコンソール /goals(v1.58) | ダッシュボードから直接エージェントへゴールを割り当て(`/goal` と同一セマンティクス)、ラウンドごとの完全な実行タイムライン(`tasks.timeline`)、その場での人間介入——ダッシュボードの全 needs_human 裁決がチャネルボタンと同じ fail-closed な `tasks.goal_decide` パスに統一 |
| 予測と検証ページ(v1.58) | LLM→LWM ループの可視化:予測 → 実行 → 観測 → 対照;ラウンドごとの予測 vs 実際(`forward.chain`);エージェントごとの予測能力判定カード(Brier + Murphy 分解、3 態の誠実ラベル);世界モデル状態バケットが初めて閲覧可能に;MAV 観点別裁決、実行記録リンク、再派遣/進捗なしシグナル、予測サブ誤差もラウンド単位で永続化 |
| チャネル OTP 候補チェーン + 設定統合(v1.58) | ログイン OTP 送信は「グローバル token 優先、次に各エージェント専用 bot token を順に試行」(重複排除・順序付き)——bot を単一エージェントへ移した後の静かな失敗を修正;エージェントのチャネル設定とチャネル管理が同一の編集ダイアログを共有;サイドバー「新機能」(`newIn`)バッジ規約導入 |
| 信念ループ(v1.59) | 外部世界に対する構造化信念の記帳(`belief_submit` / `belief_settle` / `belief_stats` MCP ツール);提出時ベースラインに対する決定論的三方向 Brier 決算、TickHub 交差検証(エージェントは現実を自己申告できない);校正統計と信念-実値対照の 2 つのプログラム的注入フック;/foresight の信念と検証タブ([46-belief-loop.md](../46-belief-loop.md)) |
| ゴール単位の契約フィールド + 自主研究(v1.59) | ゴール作成時に `duration_hours`(期限超過 → needs_human)と `risk_boundary`(空なら 5 行のベースライン)を設定可能、毎ラウンド注入され MAV の safety 観点の基準に;`/goal` は `時限:`/`邊界:` セグメント対応;構造化予測の要求チェックボックス;当日の信念を外したエージェントには夜間の自主研究ゴールを自動割り当て |
| ディスパッチエンジンのデフォルト有効化 + スケジューラ生存性(v1.59) | `[dispatch] enabled` デフォルトを true に(割り当てたゴールが箱出しで実行される)、ダッシュボードでホット切替;cron/heartbeat ループが 5 分以上停止すると `/healthz` が 503——スケジューラ全滅中もコンテナが healthy 表示だった事故を封じる |
| 二段階裁決 + 判定の強化(v1.60) | MAV 判定団の前に安価な第一段階評価器(`continue`/`candidate_complete`/`blocked`、デフォルト on;いかなる障害も完全 MAV へ降格、自動合格は絶対にしない);判定規律 4 条(反ラチェット、監査のみで証拠を自作しない、契約外拡張禁止、自己申告の完了は証拠でない);切り詰められたパネル JSON と先頭トークン `PASS` の 2 つの誤検出穴を封鎖;gap 指紋による停滞検出;早期切り上げ検出;`resume_on_restart` デフォルト `pause` |
| 差し替え可能な判定 seam(v1.60) | `[dispatch] judge = mav / evaluator_only / external / human_only`——外部判定の障害は常に MAV へ降格(より厳格、監査記録付き)、その feedback は未信頼 DATA として処理;未知の値は `mav` にフォールバック;設定→自動化にセレクタ |
| ゴール契約の凍結(v1.60) | 作成時に受け入れ基準を不変の `acceptance_criteria_baseline` として凍結、判定と評価器はこのベースラインのみを読む;エージェント身分による goal タスク受け入れ基準の変更は拒否 + 監査記録;基準なしの `/goal` には 4 要素ガイダンスと outcome 式基準の提案を付与 |
| ゴールループの人間シグナル + アドミッションキュー(v1.60) | needs_human に閉じた 6 分類の `pause_reason`(トリガー現場で静的スタンプ、LLM の記述から逆解析しない);超過進捗レポート(`progress_report_minutes`);LLM ゼロのツール連打アドバイザリ(3/5/8 段階);ephemeral spawn の上限超過は有界 FIFO キューに(デフォルト `queue`);予算枯渇時は「ベストラウンド成果物」を引き渡し(決定論的選択 + ギャップ一覧、手ぶらエスカレーションの廃止) |
| Agent Mail(v1.60) | エージェントごとのメールボックス(`/mail` ページ):Gmail API / drop folder 受信、送信は常にドラフト作成 → ApprovalBroker 確認待ち(実送信はバックグラウンドワーカーのみ)、メール内容は DATA フェンス、外部付与不可の専用 scope、エージェント横断の閲覧は組織権限判定を通過([47-agent-mail.md](../47-agent-mail.md)) |
| エージェント設定プリセット P1(v1.60) | `duduclaw preset` コマンド群 + `agent create --preset`——名前付きで再利用可能な設定バンドル;バインディング権威は `preset_bindings.toml`、解決結果はエージェントディレクトリ外へ実体化(自己改変による回避を防止)、org フィールドは値があれば全体拒否、機微セクションは静かに剥離;組み込み部門プリセット 9 種 |
| 統一アサインパネル + 計画モード + ギャラリー(v1.60) | すべての入口が同一のアサインパネルを共有(個人版にようやく主要アクション)、質問 / 委任 / 「考えてみて」の 3 モード——計画モードは 3-8 ステップの計画を生成して needs_human で承認待ち、承認後 `<execution_plan>` として 1 回だけ注入;終了したゴールへの「続きをやって」対応;インスピレーションギャラリー `/gallery` が 22 業種チームの実例をワンクリック複製カードに展開;タスク詳細は 4 タブ化(成果物/ファイル/変更/経過) |
| 成果物 provenance + 納品安全(v1.60) | `artifacts.jsonl` 5 種 origin の provenance 台帳(declared/swept/uploaded/produced/unknown;exact/inferred の帰属表示、時間窓による推測は絶対にしない);goal 受け入れ時に成果物コピーを `attachments/` へアーカイブ(canonicalize 封じ込め、20MB/100MB 上限);📎DELIVER 前の LLM ゼロ納品ゲート(ゼロバイト/magic 不一致/zip 破損はハード失敗);`[limits]` DocumentLimits が下流 3 パーサーを防御;エキスパートパック解凍の「ヘッダ虚偽申告」バイパスを修正 |
| 認証情報 P1 + secret 参照の収斂(v1.60) | `secret://keychain` と `secret://file` のローカル backend、tick ソース headers の `secret://` 対応、認証情報インベントリカード + `doctor --fix-residue`;`SecretRef`/`Secret` 型が手作り復号 7 方言を収斂——`secret://` 参照リテラルが本物の認証情報として vendor API へ送られる漏洩を修正;WhatsApp webhook 署名検証を fail-closed 化;ActionGuard 判定は 21 トークンの閉じた列挙 findings のみを受領(攻撃者制御テキストは構造的に判定プロンプトへ到達不能);MCP キーのローテーション即時反映、`denied_tools`/`allowed_tools` を MCP ディスパッチゲートでも強制 |
| 10 チャネル通知統一(v1.60) | autopilot `notify`、MCP `send_message`、リマインダーがすべて共有 `create_sender` ファクトリ経由の 10 チャネル対応に(WebChat は誠実に拒否)——一度も配信されたことのなかった autopilot Slack 通知の生きた bug と Google Chat / Teams の静かなスキップ欠陥を修正 |
| 進化計測の強化(v1.60) | AEE コミットゲートを visible / held-out の 2 次元に分離(fence-only:拒否のみ可能で昇格には使えない);チャンピオン bootstrap を同形計測に;`duduclaw evolution clear-holdout-rotation` オペレーター出口;ラウンドごとに 14 ノブのスナップショットを `aee_round` イベントへ;Code Mode Phase 0 計測ゲート(`duduclaw cost tool-loop`、4 基準の PROCEED/REJECT/INSUFFICIENT_DATA) |
| cron 曜日規約の修正(v1.61、**BREAKING**) | 数値曜日フィールドをパース時に Unix crontab 規約(0/7=日曜、1-5=月〜金)から `cron` crate の Quartz 序数へ変換、スケジューラ/heartbeat/MCP 検証/ダッシュボードが同一 normaliser を共有——従来 `* * 1-5` は日曜〜木曜に発火していた(日曜のゴースト発火 + 金曜の静かなスキップ);意図的に Quartz 規約で書かれたスケジュールはアップグレード後 1 日ずれる |
| `duduclaw migrate-from claude-code`(v1.61) | Claude Code の memory shard(→ semantic + SPO 時間記憶)、CLAUDE.md(→ エージェント wiki の context 層、注入予算を消費しない)、セッション書き起こし(ノイズ除去で人間 prompt + assistant 最終返信のみ——実測で有効シグナルは約 1.5%)の一方向インポート;すべて `origin=import`(trust ≤ 0.7)、DATA 扱い、redaction デフォルト on、インジェクションスキャン通過、skill は fail-closed のセキュリティスキャン;`--apply` なしでは何も書き込まない |
| チャネル能力テーブル(v1.61) | `channel_capabilities.rs` が 11 チャネル × 7 能力(ファイル/写真アップロード、対話ボタン、その場編集、typing、ネイティブ markdown、引用返信)+ 進捗スロットル秒数の単一権威;未対応の能力は静かな no-op から構造化ログへ |
| minimal_context コンテキスト削減(v1.61) | 公式 CLI の全 spawn にキュレーション済み `--tools` リスト + `--setting-sources project,local`(agent-file-guard hook は維持):実測で固定オーバーヘッド 35,892 → 10,974 tokens/spawn(約 69% 削減);`estimate_tokens` の CJK 再校正(約 22% の過小評価を修正);MCP `tools/list` を呼び出し元 capability でフィルタ(discoverable ⊆ callable) |
| 認証情報 P2/P3(v1.61) | 再起動不要のローテーション——アカウントプール書き込みで rotator キャッシュ無効化、Telegram は毎ポーリングで token 再解決、6 つの webhook チャネルは受信署名を per-request 検証、Odoo は次回呼び出しで再接続(Discord/Slack の常駐 WS は要再起動);spawn env を許可リストで洗浄(すべての `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD` を除去、vendor キーは呼び出し元が明示注入);エージェント単位のオプトイン `[capabilities] git_credentials`(デフォルト off)で git push 用 SSH/GPG を復元(監査付き);secret:// 収斂第 2 ラウンド(account_rotator + mcp.rs) |
| コンソールとタスクの磨き込み(v1.61) | ⌘K のソース横断コンテンツ検索(会話/成果物/記憶/wiki)、`/files` の検索 + タスクフィルタ + 期間指定、`/goals` タスクのピン留め/アーカイブ/リネーム + ページネーション(20 件ハードカット解除)、読み取り専用 `/presets` ページ、メール拒否メモ、`/goals` 詳細を 4 タブの `/tasks/:id` 正式ページへ統合 |

## 2026-08 追加(v1.53)

| 機能 | 説明 |
|------|------|
| 進化システム v3:AEE + playbook | デフォルトの進化対象を「SOUL.md の全面書き換え」から遺伝子形 playbook 行動ルールへ変更——Gate/Measure 分離、champion + matches-or-improves コミットゲート、エントリ単位の観察ウィンドウ;SOUL.md はエージェントに対して読み取り専用([38-aee-playbook-evolution.md](../38-aee-playbook-evolution.md)) |
| E1 エントリアサーション + 反 reward-hacking 監査 | 新規ルールは機械検証可能なアサーション必須。録画済み transcript に対し LLM ゼロで再生検証(`G-Assertions`);コミット前に評価問題の丸写し / 恒真表現 / 失敗隠蔽を決定論的にスクリーニング |
| タスク層フォワードモデル | goal loop 上の predict-act-verify 世界モデル:4 段階退化の統計予測(コールドスタート LLM ゼロ)、観察証拠の忠実度分級(ネイティブツールイベント / 監査ログのみ / なし)、`<state>` ブロック + (状態, 行動) 訪問グラフによる振動検出、決定論的タスクルール帰納;`[task_forward_model]`、デフォルト off |
| ディスパッチ証拠グラウンディング事前チェック | 受け入れ判定の前に LLM ゼロの証拠チェック——最終回答は実在する非エラーのツール結果と重なる必要がある;自己エコー除外リスト + 入力重複控除で自己証明を防止;`[dispatch] grounding_precheck_enabled`、デフォルト on |
| メモリ新規性ゲート | 意味層のほぼ重複した書き込みを書き込み時に拒否しテレメトリ記録(0.92 文字 n-gram cosine)——偽サプライズの蓄積防止;時間的置換・再確認パスは除外;`[memory] novelty_gate`、デフォルト on |
| 証拠必須のリフレクション | MistakeNotebook エントリはプログラム抽出の `TrajectoryEvidence` を保持;証拠のない自己申告ミスはルール統合に参加しない |
| 行動前シミュレーション承認 | `needs_human` / 承認リクエストに 3 ステップのシミュレーション軌跡を添付(15 秒上限、タイムアウト時は劣化しブロックしない);wiki 参照は読み取り専用 namespace のみ;ダッシュボードでプレビュー表示 |
| Eval 録画分離 + ブートストラップ CLI | `--record` は一時 `.mcp.json` コピーで実行(eval home + プレースホルダキー——本番への副作用ゼロ、キー漏洩なし);max-turns 暴走は `error_max_turns` として解析(評価可能な失敗ベースライン);`duduclaw eval-scaffold` は SOUL ルールから問題ドラフトを生成;`duduclaw playbook migrate-soul` は旧 SOUL ルールを playbook ドラフトへ移行 |
| 監査ログの証拠ソース化 | `tool_calls.jsonl` にマスク済み `result_text`/`input_text` を記録(3 パスシークレットマスキング、16MB ローテーション、0600);システム送信者のディスパッチ(goal-loop/cron/heartbeat/autopilot)は実行エージェントに帰属 |

## 2026-07 中盤〜後半追加(v1.33 – v1.46)

| 機能 | 説明 |
|------|------|
| 統一 LLM プロバイダーレイヤー(`duduclaw-llm`) | 4 つのネイティブプロトコル(Anthropic / OpenAI Responses / Gemini / OpenAI-compat、8 プリセット)を 1 つの正規化された request/stream 形状で扱う。`ModelRegistry` 価格表 + `FallbackRouter` クールダウン。stdio MCP クライアント + プロバイダー非依存のツールループにより API モードのエージェントも全ツールサーフェスを獲得 |
| エージェント行動 eval(`duduclaw eval`) | エージェントごとのゴールデンタスク回帰:決定論的な tool-call/regex/grounded アサーション + 任意の LLM judge。live / replay の 2 モード、CI をゲートする exit code |
| HITL ApprovalBroker | MCP ツール / autopilot / bus タスクを横断する単一の割り込み/承認プリミティブ。SQLite ベース、TTL 期限切れ = DENY(フェイルクローズ) |
| OpenTelemetry GenAI トレーシング | opt-in の `gen_ai.*` span、OTLP で Langfuse/Grafana/Jaeger/Datadog へエクスポート可能。オフ時はオーバーヘッドゼロ |
| チャネル UX レイヤー | プラットフォーム別 markdown レンダリング、typing インジケーター、8 つの外部チャネルでその場編集されるライブ todo ボード進捗 |
| 自律ゴールループ | `/goal` → 3 観点 MAV 受け入れ判定付きで完了までループ。行き詰まりはチャネルボタン付きで人間へエスカレーション([34-goal-loop.md](34-goal-loop.md)) |
| 反復カンバンラウンド | タスクボードの `revising` 状態機械 + ラウンドごとの詳細履歴 |
| 信頼された記憶と判定の強化(v1.41) | Sybil 耐性のある再確認を備えた書き込み時 origin binding、GovMem 昇格ゲート、Janus ルール保護観察、PORTICO タスクスコープの capability grant、trace-grounded な eval アサーション |
| OS ネイティブ知覚とプロアクティブケア | ファイル監視 + 前面アプリセンシング → フットプリント temporal memory(再起動耐性スナップショット)、組み込みプロアクティブケアチェック、LLM スコアリングのプロアクティブゲート、ワンクリック OS 自動化テンプレート([33-os-native-perception.md](33-os-native-perception.md)) |
| オフィスドキュメントスイート | 本物の docx/xlsx/pptx/pdf 出力、📎DELIVER プロトコル + 未宣言出力スイープ、ゲートウェイアーカイブ、LibreOffice プレビュー付きファイルページ([31-office-document-suite.md](31-office-document-suite.md)) |
| エキスパートパックエコシステム | インストール可能な AI チーム:セキュリティスキャン付きインストールパイプライン、カテゴリ/部門でグループ化された組み込み産業カタログ、LLM ガイド付きパックオーサリング、`--attach-under` 付きの部門 × ランク組織配置([32-expert-packs.md](32-expert-packs.md)) |
| レコーディング → スキル | ブラウザ(Playwright trace+HAR、シークレットはその場で秘匿処理)とデスクトップのレコーダーを、承認ゲート付き SKILL.md ドラフトへ蒸留([36-recording-to-skill.md](36-recording-to-skill.md)) |
| 写真 → デスクトップペット | ローカルで写真 → 背景除去 → ピクセル量子化 → Codex-Pets 8×9 スプライトシート。本物の常時最前面ウィンドウを動かす自律徘徊エンジン([35-photo-desktop-pet.md](35-photo-desktop-pet.md)) |
| ケイパビリティ機能トグル | 生の allow/deny ツールリストの上に置かれた 16 の平易な機能グループ。完全性ガード付きツールカタログが裏付け |
| Google Workspace / Notion / GitHub ネイティブツール | Gmail/Calendar/Drive/Sheets、Notion、GitHub のファーストパーティ MCP ツール(v1.45。設定されるまでデフォルト非表示) |
| デスクトップシェル(Tauri 2) | ゲートウェイ + ダッシュボードを包むネイティブウィンドウ、トレイ、ゲートウェイピッカー、透明なデスクトップペットオーバーレイウィンドウ |
| インタラクションペーシングガード | 会話履歴フレーミング + 常時有効なペーシングルールにより、挨拶が直前のツール多用タスクを再トリガーしないようにする |

## 2026-07 追加(v1.24.0 以降)

| 機能 | 説明 |
|------|------|
| Aider 式コードシンボルグラフ(`code_map` MCP ツール) | tree-sitter シンボルグラフを HippoRAG-lite Personalized-PageRank エンジン上で実行し、クエリとの関連度でリポジトリのソースファイルをランク付け |
| セマンティックベクトル記憶(`w_vec`) | FTS/graph に加えた第三の re-rank シグナル。依存ゼロ・CJK 安全の `NgramHashEmbedder`、`DUDUCLAW_SEMANTIC_VECTORS=1` で有効化 |
| セッション横断ユーザープロファイル | ユーザーごとの嗜好 traits(temporal supersession)→ セッション安定な `## About This User` を返信に注入。`user_profile_record` / `user_profile_get` MCP ツール |
| GDPR エクスポート/消去 | `duduclaw gdpr export\|erase <contact>` が記憶(triple + 本文言及 + key_facts、4 テーブルのカスケード、SHA-256 仮名 tombstone)**と**セッションストア(`<channel>:<chat_id>` プレフィックス)を対象 |
| 記憶 PPR ベンチ | `duduclaw memory bench` — P50/P95 レイテンシ + パーティション推奨(LightRAG 計測ゲート) |
| 予算サーキットブレーカー | エージェント単位のスライディングウィンドウ上限(`[budget] daily_cap_cents`)。上限到達で choke-point にて LLM 呼び出しを遮断。`budget_events.jsonl` |
| バーンレート異常検知 | エージェントごとの日次支出に対し移動平均+標準偏差で外れ値を検出(`cost_anomaly.rs`) |
| 監査エクスポート + SIEM sink | `duduclaw audit` — JSONL 監査ログを正規化し NDJSON / webhook へストリーム |
| 送信 guardrail フック | opt-in `[guardrails]` — 送信前に秘密情報の漏洩 / インジェクション反響 / 禁止フレーズ / PII をスキャン |
| CI レッドチームスキャン | `duduclaw redteam` — `CONTRACT.toml` の `must_not` から jailbreak バリアントを生成し input-guard に通す |
| セキュリティ姿勢レポート | `duduclaw security` — 有効な防御の重み付きチェックリスト |
| バックアップ / リストア | `duduclaw backup` / `restore` — タイムスタンプ付きホームアーカイブ + SHA-256 サイドカー(リストア時に検証) |
| セッションリプレイ | `duduclaw session replay <id>` — セッションのターンを順に出力(`--tools` 併用可) |
| MCP Bridge | `[[mcp.external]]` — 外部 MCP サーバーをマウント。deny-by-default のツールフィルタ + `env://` / `secret://` 認証情報。各 SaaS のレシピは `guides/mcp-bridge.md` |
| Secret manager バックエンド | 1Password Connect + Infisical アダプタ。`secret://<backend>/<name>` の解決を MCP Bridge に配線 |
| MCP/skill 信頼ティア分け | リポジトリの最終 push 時期 + オーナー種別から official / active / orphan を判定 |
| Email チャネル(部分) | 非同期 SMTP 送信(`lettre`、loopback で実地検証)+ RFC822 受信パース。IMAP ポーリング + チャネルライフサイクルは PENDING-LIVE |
| コミュニケーションチャネル | 現在**9 個**(下記 7 個に Google Chat + Microsoft Teams を追加) |

---

## コアアーキテクチャ

| 機能 | 説明 |
|------|------|
| マルチランタイム AI エージェントプラットフォーム | 統一 `AgentRuntime` trait — Claude / Codex / Gemini / Antigravity (`agy`) / OpenAI-compat 5 バックエンド自動検出 |
| MCP Server（JSON-RPC 2.0）| stdin/stdout 経由で AI Runtime に 80+ ツールを公開。`<agent>/.mcp.json` に登録（Claude CLI `-p` はプロジェクトレベルのみ読取）、起動時に自動生成/修復 |
| ACP/A2A Server | 2 コマンド：`duduclaw acp` — IDE agent panel 向け Agent Client Protocol v1（Zed / JetBrains / nvim；`initialize` / `session/new` / `session/prompt` ストリーミング、未設定時は `AUTH_REQUIRED`）；`duduclaw acp-server` — A2A プロトコル（`agent/discover` / `message/send` / `tasks/*`、`.well-known/agent.json` AgentCard） |
| エージェントディレクトリ構造 | `.claude/`, `.mcp.json`, `SOUL.md`, `CLAUDE.md`, `CONTRACT.toml`, `agent.toml`, `wiki/`, `SKILLS/`, `memory/`, `tasks/`, `state/` |
| サブエージェントオーケストレーション | `create_agent` / `spawn_agent` / `list_agents` + `reports_to` 階層 + D3.js OrgChart + 「## Your Team」自動注入 |
| DelegationEnvelope | 構造化受け渡しプロトコル — context / constraints / task_chain / expected_output |
| TaskSpec ワークフロー | 多段階タスク計画 — 依存認識スケジューリング、自動再試行（3x）、再計画（2x）、永続化 |
| 長返信ページング | チャネル byte budget を超えるサブエージェント返信を `channel_format::split_text` で分割、`📨 **agent** 的回報 (1/N)` ラベル |
| 孤立レスポンス復旧 | dispatcher 起動時 `reconcile_orphan_responses` が `bus_queue.jsonl` の crash/Ctrl+C/hotswap 残留 callback を原子的に再生 |
| ファイルベース IPC | `bus_queue.jsonl` によるエージェント間委任、最大 5 ホップ追跡 |
| Per-Agent Channel Token | `get_agent_channel_token` が per-agent `bot_token_enc` を優先読取（Discord スレッド間 bot の 401 を修正）|

## マルチランタイム

| 機能 | 説明 |
|------|------|
| Claude Runtime | Claude Code SDK (`claude` CLI) + JSONL ストリーミング + `--resume` ネイティブマルチターン |
| Codex Runtime | OpenAI Codex CLI + `--json` ストリーミング、`AGENTS.md` で system prompt を渡す |
| Gemini Runtime | Google Gemini CLI + `--output-format stream-json`、`GEMINI_SYSTEM_MD` env で system prompt、`--approval-mode yolo`。Google が 2026-06-18 に個人向け Gemini CLI を廃止後、有料 `GEMINI_API_KEY` 利用者向けに維持 |
| Antigravity Runtime（v1.24.0）| Google Antigravity CLI（`agy`、2026-06-18 の Gemini CLI 後継）、ワンショット `agy -p --dangerously-skip-permissions --print-timeout 300s` で駆動。バイナリ自動解決（PATH → `~/.local/bin/agy`）；`--system` フラグがないため system prompt + 履歴をプロンプトに埋め込み（CJK セーフ）；認証 `ANTIGRAVITY_API_KEY`；エージェントのディレクトリを agy の `trustedWorkspaces` に事前登録（クロスプロセスロック）し、ヘッドレスの信頼ダイアログでのハングを回避；トークン使用量は推定（print モードは統計なし）|
| OpenAI 互換 Runtime | HTTP エンドポイント（MiniMax / DeepSeek 等）REST API |
| RuntimeRegistry | インストール済み CLI の自動検出、per-agent `[runtime]` 設定 |
| クロスプロバイダーフェイルオーバー | `FailoverManager` ヘルス追跡、クールダウン、再試行不可エラー検出 |

## セッションメモリスタック（v1.8.1 + v1.8.6）

| 機能 | 説明 |
|------|------|
| ネイティブマルチターン | Claude CLI `--resume` + SHA-256 決定論的 session ID + history-in-prompt フォールバック |
| ターントリミング（Turn Trimming） | >800 chars → 先頭 300 + 末尾 200 + `[trimmed N chars]`、CJK セーフ文字スライス |
| プロンプトキャッシュ戦略 | Direct API "system_and_3" ブレークポイント、マルチターン ~75% ヒット率 |
| 圧縮サマリー注入 | 圧縮後サマリー（role=system）を system prompt に注入、会話ターンではない |
| Instruction Pinning | 最初のユーザーメッセージ → 非同期 Haiku 抽出 → `sessions.pinned_instructions` → system prompt 末尾（U字型注意） |
| Snowball Recap | 各ターンの user message 先頭に `<task_recap>` を付加、LLM コストゼロ |
| Clarification 累積 | エージェントの質問 + ユーザー回答 → pinned instructions に追加（≤1000 文字） |
| P2 Key-Fact Accumulator | 実質的なターン毎に 2-4 事実 → `key_facts` FTS5 テーブル → top-3 注入（~100-150 tokens vs MemGPT 6,500、−87%） |
| CLI 軽量パス | `call_claude_cli_lightweight()` — 25-40% コスト削減 |
| 安定化フラグ | `--strict-mcp-config` + `--exclude-dynamic-system-prompt-sections`（10-15% token 削減）；`--bare` は v1.8.11 で削除（OAuth キーチェーンを破壊） |
| CJK セーフ文字列スライス | `duduclaw_core::truncate_bytes` / `truncate_chars` が 31 箇所の unsafe byte-index スライスを置換 |

## 通信チャネル（7種）

| チャネル | プロトコル |
|----------|------------|
| Telegram | ロングポーリング、ファイル/写真/スタンプ/音声、forums/topics、mention-only、音声転記 |
| LINE | Webhook + HMAC-SHA256、スタンプ、per-chat 設定 |
| Discord | Gateway WebSocket、スラッシュコマンド、ボイスチャンネル（Songbird）、auto-thread（v1.8.14 で thread session id 漂流修正）、embed 返信 |
| Slack | Socket Mode、mention-only、thread 返信 |
| WhatsApp | Cloud API |
| Feishu | Open Platform v2 |
| WebChat | 組み込み `/ws/chat` WebSocket + React フロントエンド |
| チャネルホットスタート/ストップ | Dashboard 駆動の動的起動/停止 |
| Generic Webhook | `POST /webhook/{agent_id}` + HMAC-SHA256 署名検証 |
| メディアパイプライン | 自動リサイズ（max 1568px）+ MIME 検出 + Vision 統合 |
| スタンプシステム | LINE スタンプカタログ + 感情検出 + Discord emoji 対応 |
| チャネル失敗追跡 | `channel_failures.jsonl` + `FailureReason` 列挙 |
| Discord Gateway 強化（v1.9.2）| 本物の op 6 RESUME — 再接続を跨いで `session_id` + `resume_gateway_url` + sequence を保持；`select!` 停滞ウォッチドッグが 2× ハートビート沈黙で中断（18 分ゾンビ修正）；ハートビート channel 容量 1→16 を `try_send`；op 9 は `d.bool` で RESUME か IDENTIFY を選択し 1-5s jitter；close code 4007/4009/4003 で session クリア；バックオフ上限 300s→60s；`RESUMED` dispatch 対応 |

## 進化システム

> **進化システム v3（2026-08-06）**:デフォルトの進化対象は「`SOUL.md` の
> 全面書き換え」から **playbook**（小粒度で個別に退役可能な遺伝子形ルール;
> `SOUL.md` はデフォルトでエージェントに読み取り専用）へ移行しました。
> 下表の GVU² 系の行（デュアルループ / 4+2 層検証 / SOUL.md バージョン管理）
> は**非デフォルトの legacy パス**（`agent.toml [evolution]
> legacy_soul_evolution = true`）の説明です。現行デフォルト（AEE、
> Gate/Measure 分離、champion + matches-or-improves、エントリ単位観察
> ウィンドウ）は [38-aee-playbook-evolution.md](../38-aee-playbook-evolution.md)
> と [evolution-engine.md](../../architecture/evolution-engine.md) 第 12 章を参照。

| 機能 | 説明 |
|------|------|
| 予測駆動エンジン | Active Inference + Dual Process Theory、約 90% LLM コストゼロ |
| デュアルプロセスルーター | System 1（ルール）/ System 2（LLM リフレクション） |
| AEE（v3 デフォルト） | Agentic Evolution Engine — Generator 内ループ（≤3 ラウンド）→ Gate（決定論的・拒否権あり）/ Measure（スコア・拒否権なし）分離 → champion + matches-or-improves コミットゲート → エントリごとにリンクされた eval case に対して confirm/rollback |
| Playbook（v3 デフォルト） | 遺伝子形行動ルール（category/signals_match/eval_cases/success_streak）、既存 rule_lifecycle ストアの拡張、0.92 cosine 重複排除、容量 + 失効/アーカイブライフサイクル |
| GVU² デュアルループ（legacy） | 外側ループ（Behavioral GVU — SOUL.md 書き換え）+ 内側ループ（Task GVU — 即時再試行）;`legacy_soul_evolution = true` でオプトイン |
| 4+2 層検証（legacy） | L1-Format / L2-Metrics / L2.5-MistakeRegression / L3-LLMJudge / L3.5-SandboxCanary / L4-Safety |
| MistakeNotebook | ループ間エラー記憶 — 失敗パターン記録、退行防止;エントリは決定論的 `TrajectoryEvidence`（どのツール/アサーションが失敗したか）を保持し、証拠のない自己申告診断はリフレクション統合に参加しない（v3） |
| SOUL.md バージョン管理（legacy） | 24h 観察期間 + アトミックロールバック + SHA-256 フィンガープリント — legacy GVU パスに適用;SOUL.md サイズ上限デッドロックはガード付き consolidate 書き換えで解除され、エージェントが凍結しなくなった（v3 Phase 0） |
| MetaCognition | 100 予測毎に誤差閾値を自己校正、対称的な引き上げ規則を追加し閾値の一方向ドリフトを解消（v3 Phase 0） |
| Adaptive Depth | MetaCognition 駆動の GVU 反復深度（3-7 ラウンド） |
| Deferred GVU（legacy） | 勾配累積 + 遅延再試行（最大 3 deferral、72h、9-21 実効ラウンド） |
| 停滞検出器（v3） | 30 分毎に `evolution.db` の連続拒否 / D 日間ゼロ適用 / 拒否理由反復シグナルをスキャンし、Activity Feed + ダッシュボードへ通知 |
| ConversationOutcome | LLM ゼロの会話結果検出、zh-TW + en |
| Agent-as-Evaluator | 独立 Evaluator Agent（Haiku コスト管理）による対抗的検証 |
| Orchestrator テンプレート | 5 ステップ計画（Analyze → Decompose → Delegate → Evaluate → Synthesize）|

## Wiki 知識レイヤー（v1.8.9）

| 機能 | 説明 |
|------|------|
| 4 層アーキテクチャ | L0 Identity / L1 Core / L2 Context / L3 Deep — Vault-for-LLM 由来 |
| 信頼度加重 | `trust` (0.0-1.0) frontmatter；検索は trust-weighted score でランク |
| 自動注入 | `build_system_prompt()` が CLI / channel / dispatcher 3 パスで L0+L1 を WIKI_CONTEXT に注入 |
| FTS5 全文索引 | SQLite `unicode61` tokenizer（CJK 対応）、書込/削除で自動同期、`wiki_rebuild_fts` で手動再構築 |
| 知識グラフ | `wiki_graph` MCP ツールが BFS 制限付き Mermaid 図を出力、レイヤー別ノード形状 |
| Dedup 検出 | `wiki_dedup` — タイトルマッチ + タグ Jaccard 類似度（≥0.8） |
| 逆 backlink 索引 | `related` frontmatter + body markdown リンクをスキャン |
| 検索フィルタ | `min_trust` / `layer` / `expand`（1-hop related/backlink 拡張） |
| 共有 Wiki | `~/.duduclaw/shared/wiki/` — 組織横断 SOP/ポリシー/仕様、`wiki_visible_to` 可視性制御；MCP ツール `shared_wiki_ls/read/write/search/delete/stats`、`wiki_share`；`.scope.toml` SoT ポリシー（「アイデンティティとアクセス」参照）|
| CLAUDE_WIKI テンプレート | 新規エージェント作成時に CLAUDE.md へ同梱、LLM に wiki MCP ツールの使い方を教示 |

## スキルエコシステム

| 機能 | 説明 |
|------|------|
| 7 段階ライフサイクル | Activation → Compression → Extraction → Reconstruction → Distillation → Diagnostician → Gap Analysis |
| GitHub ライブインデックス | Search API + 24h ローカルキャッシュ + 加重検索 |
| スキルマーケット | Web ダッシュボード、インストール、セキュリティスキャン |
| スキル自動合成 | ギャップアキュムレーター → エピソード記憶から合成（Voyager 由来）→ サンドボックス試用（TTL） → クロスエージェント卒業 |
| スキル合成スケジューラ（W19-P1、v1.22.0）| 「会話 → skill」抽出を一定間隔で自律実行 — `config.toml [skill_synthesis] auto_run / dry_run / interval_hours / lookback_days` + ダッシュボード `skill_synthesis.get/update` RPC；`skill_synthesis_threshold` は `u32` カウント（registry スキャンが `0.7` を拒否する不具合を修正）|
| Skill セキュリティスキャナー（Rust ネイティブ）| `skill_lifecycle::security_scanner` が候補スキルをスキャン、Python 依存なし |

## ローカル推論エンジン

| 機能 | 説明 |
|------|------|
| llama.cpp | Metal/CUDA/Vulkan/CPU（`llama-cpp-2` crate） |
| mistral.rs | Rust ネイティブ、ISQ、PagedAttention、Speculative Decoding |
| OpenAI 互換 HTTP | Exo/llamafile/vLLM/SGLang |
| 信頼度ルーター | LocalFast / LocalStrong / CloudAPI 3 層 + CJK-aware トークン推定 |
| InferenceManager | マルチモード自動切替：Exo P2P → llamafile → Direct → OpenAI-compat → Cloud API |
| llamafile マネージャ | サブプロセスライフサイクル、6 OS でゼロインストール |
| Exo P2P クラスタ | 分散推論、235B+ モデルを複数マシンで実行 |
| MLX Bridge | Apple Silicon の `mlx_lm` + LoRA ローカルリフレクション |
| モデル管理 | `model_search`（HuggingFace）/ `model_download`（resume + mirror）/ `model_recommend`（ハードウェア認識） |

## 圧縮エンジン

| 機能 | 説明 |
|------|------|
| Meta-Token（LTSC） | Rust ネイティブ無損失 BPE-like、構造化入力で 27-47% 圧縮率 |
| LLMLingua-2 | Microsoft トークン重要度剪定、2-5x 損失圧縮 |
| StreamingLLM | Attention sink + スライディングウィンドウ KV-cache、無限長会話 |
| 戦略セレクタ | `compress_text` の `strategy` パラメータ — `meta_token` / `llmlingua` / `streaming_llm` / `auto` |

## 音声パイプライン

| 機能 | 説明 |
|------|------|
| ASR | Whisper.cpp（ローカル）/ SenseVoice ONNX（ローカル）/ OpenAI Whisper API / Deepgram（ストリーミング） |
| TTS | Piper ONNX（ローカル）/ MiniMax T2A / Edge TTS / OpenAI TTS |
| VAD | Silero ONNX |
| オーディオデコード | symphonia：OGG Opus / MP3 / AAC / WAV / FLAC → PCM |
| Discord Voice | Songbird 統合 |
| LiveKit | WebRTC マルチエージェント音声ルーム |
| ONNX Embedding | BERT WordPiece tokenizer + ONNX Runtime |

## セキュリティ

| 機能 | 説明 |
|------|------|
| 3 段階防御 | 決定論的ブラックリスト（<50ms）/ 難読化検出（YELLOW+）/ Haiku AI 判定（RED のみ） |
| 脅威レベル状態機械 | GREEN → YELLOW → RED 自動エスカレート、24h 無イベントで -1 |
| Ed25519 認証 | チャレンジレスポンス WebSocket 認証 |
| AES-256-GCM | API キーの保存時暗号化、per-agent 隔離 |
| Prompt Injection スキャナ | 6 ルールカテゴリ + XML 区切りタグ保護 |
| SOUL.md ドリフト検出 | SHA-256 フィンガープリント比較 |
| CONTRACT.toml | 行動境界 + `duduclaw test` レッドチーム CLI（9 シナリオ）；全ランタイムの system prompt へ自動注入 |
| RBAC | 役割ベースアクセス制御マトリクス |
| 統一監査ログ | `audit.unified_log` が `security_audit.jsonl` / `tool_calls.jsonl` / `channel_failures.jsonl` / `feedback.jsonl` を統合 |
| JSONL 監査ログ | 非同期書込、Rust `AuditEvent` スキーマ互換 |
| Unicode 正規化 | NFKC で同形異字攻撃を検出 |
| Action Claim Verifier | ツール実行クレームの署名検証 |
| コンテナサンドボックス | Docker (Bollard) / Apple Container / WSL2 — `--network=none`、tmpfs、read-only rootfs、512MB 上限 |
| シークレット漏洩スキャナ | 20+ パターン（Anthropic/OpenAI/AWS/GitHub/Slack/Stripe/DB URL 等） |
| 機密データのリダクション（RFC-23、v1.14.0）| `duduclaw-redaction` crate — 内部データ（Odoo / shared wiki / file tools）を `<REDACT:CATEGORY:hash8>` トークンに置換してから LLM へ送り、信頼境界（user channel reply、許可リストツールの egress）で自動復元；AES-256-GCM SQLite vault（per-agent 32-byte key、0o600）、TTL 7d の 2 段階 GC、5 つの組み込みプロファイル、5 層の enable/disable リゾルバ、JSONL 監査ログ 10MB ローテーション |

## メモリシステム

| 機能 | 説明 |
|------|------|
| エピソード/意味分離 | Generative Agents 3D 加重検索（Recency × Importance × Relevance） |
| FTS5 全文検索 | SQLite 標準搭載 |
| ベクトル索引 | Embedding セマンティック検索（ONNX BERT / Qwen3-Embedding） |
| メモリ減衰スケジューラ | 日次バックグラウンド — 低重要度 + 30 日以上アーカイブ、アーカイブ + 90 日以上完全削除 |
| 認知メモリ MCP ツール | `memory_search_by_layer` / `memory_successful_conversations` / `memory_episodic_pressure` / `memory_consolidation_status` |
| フェデレーションメモリ | エージェント横断知識共有（Private / Team / Public） |
| Key-Fact Accumulator | `key_facts` + FTS5 — セッション横断の軽量メモリ（セッションメモリスタック参照） |
| Temporal Memory（F1、v1.19.0）| `memories` に冪等マイグレーションで時系列/ナレッジグラフ列（`valid_from`/`valid_until`/`superseded_by`/`supersedes`/`subject`/`predicate`/`object`/`confidence`/`metadata`）を追加；`store_temporal()` が同一 `(agent, subject, predicate)` を自動コンフリクト解決し supersession chain を連結；`search()` はデフォルトで現行有効行のみフィルタ；`get_history()` / `get_at()` がチェーンとポイントインタイムを提供 |
| Reflexion Loop（F2、v1.19.0）| 既存 `MistakeNotebook` をブリッジ — F2a は未解決の最近のミスを回答プロンプトに注入（`## Past Mistakes to Avoid`、CJK セーフ照合 + recency フォールバック）；F2b は同一 `MistakeCategory` のミス ≥3 件を 1 つの意味メモリルールに統合（`reflexion.rs`）し元を解決済みに。トリガー = `ErrorCategory` Significant/Critical（MetaCognition 適応） |
| `memory_fetch_batch`（F3、v1.19.0）| MCP ツール + `get_by_ids` が ≤100 件を ID で一括取得（名前空間/所有権を強制、部分ヒット → `missing_ids`） |
| Bi-temporal + build-time provenance（D1）| `memories` に冪等マイグレーションで `ingested_at`（transaction-time 軸、world-time の `valid_from` とは別）＋ `invalidated_by_event`/`invalidated_at`（どの source_event が行をいつクローズしたか）を追加。`store_temporal()` の置換は world-time の `valid_from` で決定（順不同に強い — 先行する事実は現行を乱さず有界の履歴セグメントとして挿入；`valid_from` なしの書き込みは従来の取り込み順の挙動を維持）；同一事実の再観測は行を追加せず**再確認**（metadata `reaffirmed_by`、≤20、＋ `access_count` 加算） |
| `memory_get_history` / `memory_get_at`（D1）| 時系列読み取り API の MCP 露出 — 完全な置換チェーン（provenance 列付き）と `(subject, predicate)` トリプルのポイントインタイム参照（scope `memory:read`） |
| `memory_invalidate_by_origin`（D1）| ソースロールバックのプリミティブ — **厳密な** `origin` のすべての現在有効な事実を失効（削除はしない、任意でカットオフ以降に限定）、`origin_trust ≤ 0.1` を `derived_from` の子孫へカスケード；履歴は保持（`invalidated_by_event = "origin_purge"`）。scope `admin` |
| グラフ検索の進化（D3）| HippoRAG-lite graph が4つの fail-safe な改良を獲得（未使用時はバイト単位で同一）：**(1)** per-agent 永続グラフキャッシュ（`RwLock`）、トリプル変更書き込みごとに加算される per-agent 世代カウンタで無効化、`GRAPH_CACHE_MIN_TRIPLES = 500` 超のみ有効；**(2)** `entity_alias(agent_id, canonical, alias)` によるエンティティエイリアス統合 — 構築＋シード前に表層形を1ノードへ畳み込み、正規化＋チェーン平坦化；**(3)** エッジに付帯する述語エッジラベル（PPR 不変）が `engine.export_graph(agent, limit)` → シリアライズ可能な `{nodes, edges}` スナップショット（隔離事実はフラグ付き）に供給し D6 キュレーション UI へ；**(4)** オプトインの embedding seeding（`graph_embed_seed`）— PPR シード ＝ whole-word FTS ∪ クエリ埋め込み最近傍エンティティベクトル（同一モデル cosine、top-k、遅延 `entity_embedding` キャッシュ）、デフォルトオフ |
| `memory_alias_add` / `memory_alias_list`（D3）| エンティティエイリアスを管理する MCP ツール — add は `alias` を `canonical` エンティティに畳み込む（scope `memory:write`）、list は `(canonical, alias)` ペアを返す（scope `memory:read`）；名前空間分離 |
| Decision Continuity（RFC-24、v1.23.0）| エージェントが列挙式の選択肢（案 A/B/C）を提示した際、各選択肢を Temporal Memory の **semantic** 層に永続化（会話圧縮から独立）し、未決事項をターンごとに再注入；後から「案 C で」（別ターン / セッション / プロセス）と言われても推測ではなく永続状態から解決。検出は決定論的でゼロ LLM；`decision_resolve` / `decision_list` MCP ツール + ダッシュボードパネル + Prometheus カウンタ；`[memory] decision_continuity = true` でエージェント単位の opt-in（TTL `decision_ttl_days`、既定 7）|

## Git Worktree 分離（v1.6.0）

| 機能 | 説明 |
|------|------|
| L0 分離レイヤー | タスク毎の git worktree — コンテナサンドボックスより軽量、並行エージェントのファイル衝突防止 |
| アトミックマージ | dry-run 事前チェック → abort → クリーンなら実マージ；グローバル `Mutex` 保護 |
| Snap ワークフロー | create → execute → inspect → merge/cleanup；純粋関数の意思決定ロジック |
| フレンドリーブランチ名 | `wt/{agent_id}/{adjective}-{noun}`、50×50 ワードリスト |
| copy_env_files | パス走査 jail + symlink 拒否 + 1MB サイズ上限 |
| AgentExitCode | 構造化終了コード — Success / Error / Retry / KeepAlive |
| リソース上限 | エージェント毎 5 個、全体 20 個 |

## アカウントとコスト管理

| 機能 | 説明 |
|------|------|
| マルチアカウントローテーション | OAuth + API Key、4 戦略（Priority/LeastCost/RoundRobin/Failover） |
| 双方の dispatch 経路 | サブエージェント dispatcher もチャネル返信もローテーターを経由 |
| CostTelemetry | SQLite トークン追跡 + キャッシュ効率分析 + 200K 価格クリフ警告 |
| 予算マネージャ | アカウント毎月上限 + クールダウン + 適応ルーティング（cache_eff <30% → ローカル） |
| Direct API | CLI バイパス、`cache_control: ephemeral`、95%+ キャッシュヒット率 |
| 失敗分類 | `FailureReason` 列挙 + カテゴリ別 zh-TW メッセージ + `channel_failures.jsonl` |
| バイナリ探索 | `which_claude()` / `which_claude_in_home()` が Homebrew / Bun / Volta / npm-global / `.claude/bin` / `.local/bin` / asdf / NVM を探索 |

## ブラウザ自動化

| 機能 | 説明 |
|------|------|
| 5 層ルーター | API Fetch / 静的スクレイプ / ヘッドレス Playwright / サンドボックスコンテナ / Computer Use |
| 能力ゲーティング | `agent.toml [capabilities]` はデフォルト拒否 |
| Browserbase | クラウドブラウザ（L5 代替） |
| bash-gate.sh | Layer 1.5 allowlist（`DUDUCLAW_BROWSER_VIA_BASH=1` 必須） |

## コンテナサンドボックス

| 機能 | 説明 |
|------|------|
| Docker | Bollard API、全プラットフォーム |
| Apple Container | macOS 15+ ネイティブ |
| WSL2 | Windows Linux サブシステム |

## スケジューリング

| 機能 | 説明 |
|------|------|
| CronScheduler | `cron_tasks.jsonl` + `cron_tasks.db` 永続化（v1.8.12）、`schedule_task` MCP スキーマ修正（`agent_id` + `name` を含む） |
| ReminderScheduler | 一度限りのリマインダー（相対 `5m`/`2h`/`1d` または ISO 8601）、`direct` / `agent_callback` モード |
| HeartbeatScheduler | エージェント毎統一スケジューリング — バスポーリング + GVU サイレンスブレイカー + cron |
| スケジューラレベルのタスクボードプル（v1.9.3）| `poll_assigned_tasks` を `HeartbeatScheduler::run` tick へ移動 — 30s 毎に全エージェントレジストリを走査（`enabled=false` をスキップしない）；1 時間 LIKE-marker クールダウンでスタンピード防止 |
| `duduclaw evolution finalize` CLI（v1.9.1）| 既に終了しているはずの SOUL.md 観察ウィンドウのワンショット回収；`--dry-run` / `--agent` フィルタ；30 分 `ObservationFinalizer` バックグラウンドタスクのバックストップ |

## タスクボードと Activity Feed

| 機能 | 説明 |
|------|------|
| タスクボード | SQLite バックエンドのタスク管理 — status / priority / assignment 追跡 |
| Dashboard RPC | `tasks.list/create/update/remove/assign`、`activity.list`（Web UI 向け）|
| Agent MCP ツール | `tasks_list`、`tasks_create`、`tasks_update`、`tasks_claim`、`tasks_complete`、`tasks_block`、`activity_list`、`activity_post` — エージェントが自身のキュー把握、作業クレーム、進捗投稿 |
| リアルタイム Activity Feed | WebSocket ストリーミングの activity イベント |
| システムプロンプト注入 | 保留タスク（最大 5 件）をエージェント system prompt へ自動注入 |

## Autopilot ルールエンジン

| 機能 | 説明 |
|------|------|
| イベントバス | `tokio::broadcast`（容量 8192）— `TaskCreated` / `TaskStatusChanged` / `ChannelMessage` / `AgentIdle` / `CronTick` |
| ルール条件 | `all` / `any` + `eq/neq/in/gt/lt/contains` 演算子 |
| アクション型 | `delegate`（bus task をエンキュー）、`notify`（チャネル）、`run_skill`（skill 名 + ターゲットを alphanumeric allowlist + `canonicalize()` パス封じ込めで検証）|
| ルール CRUD | Dashboard RPC `autopilot.list/create/update/remove/history` + agent MCP `autopilot_list`；書込時に構造検証 |
| 3 状態サーキットブレーカー | ルール毎 `Closed` / `Open` / `HalfOpen` — 60s 内 10 回発火で Open（60s クールダウン）、その後 HalfOpen probe；自己強化ループを防止；遷移は history + Activity Feed に記録 |
| events.db ブリッジ | SQLite（WAL + 単調増加 id + 7 日 prune）が旧 `events.jsonl` を置換 — rotation race・partial-line ハザードなし |

## 信頼性とガバナンス

| 機能 | 説明 |
|------|------|
| Durability フレームワーク（`duduclaw-durability`、v1.9.4）| 5 つの柱 — `idempotency`（key ベース重複排除）、`retry`（指数バックオフ + jitter）、`circuit_breaker`（3 状態 + `probe_inflight` 計上）、`checkpoint`（再開可能タスク進捗）、`dlq`（デッドレターキュー）。gateway LLM フォールバック + 永続 cron で使用 |
| ガバナンス層（`duduclaw-governance`、v1.9.4）| `PolicyRegistry` — YAML ロード + ホットリロード + agent 優先マージ + フェイルセーフ（不正ポリシーはスキップ、不正 YAML で panic しない）。4 種の `PolicyType` — Rate / Permission / Quota / Lifecycle |
| クォータマネージャ | agent 毎 / ポリシー毎の soft + hard クォータ強制；`error_codes.rs` がガバナンスエラー（QUOTA_EXCEEDED / POLICY_DENIED / ...）を標準化。デフォルトセットは `policies/global.yaml`（例：`default-rate-mcp` 200/min）|
| LLM フォールバックチェーン（`gateway/llm_fallback.rs`、v1.9.4）| プライマリの timeout/503/429/overloaded がフォールバックモデルへ自動切替；純関数 `is_llm_fallback_error` / `should_attempt_model_fallback` をユニットテスト；hard-deadline アームが `Err("hard timeout")` を返しフォールバックを確実に発動 |
| Evolution Events システム（v1.9.4）| 30+ イベントスキーマ（`schema.rs`）、非同期 batch+retry emitter（`emitter.rs`）、クエリインターフェース（`query.rs`）、信頼性保証（`reliability.rs`）；HTTP エンドポイントを Web `ReliabilityPage` に表示 |

## アイデンティティとアクセス

| 機能 | 説明 |
|------|------|
| Identity Resolution（`duduclaw-identity`、RFC-21 §1、v1.11.0）| `IdentityProvider` async trait — `WikiCacheIdentityProvider`（`shared/wiki/identity/people/*.md`）、`NotionIdentityProvider`（Notion `databases/query` + `field_map`）、`ChainedProvider`（cache → upstream、障害時は優雅にデグレード）|
| `identity_resolve` MCP ツール | `Scope::IdentityRead` でゲート、標準 `ResolvedPerson` レコードを返却 |
| Sender 自動注入 | チャネル返信が XML 区切りの `<sender>` ブロックを system prompt へ注入（ターン毎に 1 回解決）、SOUL.md「非メンバー拒否」ルールをデータ駆動化 |
| 共有 Wiki SoT ポリシー（RFC-21 §3、v1.11.0）| `~/.duduclaw/shared/wiki/.scope.toml` が名前空間所有権を宣言 — `agent_writable`（デフォルト）、`read_only { synced_from }`、`operator_only`；`shared_wiki_write` / `shared_wiki_delete` が遵守；`wiki_namespace_status` が現行ポリシーを公開；ファイル欠如/不正 ⇒ フェイルセーフでポリシーなし |

## Live Forking（RFC-26）

| 機能 | 説明 |
|------|------|
| Live Run Forking（`duduclaw-fork`）| pydantic-deepagents 由来の実行中ブランチング — 複数の継続を並行探索 |
| AI Judge | 並行ブランチをスコアリングし最良の継続を選択 |
| 予算制御 | `budget.rs` が fork fan-out / コストを制限 |

## CLI ランタイム（PTY Pool）

| 機能 | 説明 |
|------|------|
| クロスプラットフォーム PTY Pool（`duduclaw-cli-runtime`、v1.15.0）| 本物のインタラクティブ `claude` REPL を駆動（Win 10 1809+ は ConPTY、Unix は `portable-pty` 経由 openpty）、sentinel-framed in-band レスポンスプロトコル — Anthropic が OAuth サブスク口座向けに `claude -p` をブロックした問題に対応。デフォルトオフ、per-agent オプトイン `[runtime] pty_pool_enabled = true` |
| Worker Supervisor（`duduclaw-cli-worker`）| `[runtime] worker_managed = true` でゲートされる out-of-process worker サブプロセス；SIGTERM/SIGKILL を gateway の優雅シャットダウンに連動 |
| `pty_runtime.rs` アダプタ | `RuntimeMode::{FreshSpawn, PtyPool}` per-agent ルーティング、`acquire_and_invoke` サーフェス；OAuth → インタラクティブ REPL、API-key → `oneshot_pty_invoke + claude -p` |
| Claude 固定の解除（v1.24.0）| `CliKind::Antigravity` を追加；`which_codex` / `which_gemini` / `which_agy` の探索（`which_claude` と並列）；`resolve_program` + worker `spawn_session_default` が 4 つの CliKind すべてを解決（`None`/reject なし）；`cli_kind_for_provider()` が `[runtime] provider` から PtyPool の種別を導出し、ハードコードされた 2 箇所の `CliKind::Claude` を置き換え。対話型 REPL は設計上 Claude 専用のまま（非 Claude プロバイダはワンショット `runtime_dispatch` 経路）|
| Runtime ステータスエンドポイント | `GET /api/runtime/status` loopback 限定 JSON（Phase 8.5）|
| 可観測性 | `pty_pool_*` Prometheus カウンタ（acquires / cache-hit / spawn / eviction / invoke outcomes / duration histogram）、`worker_health_misses_total`、`worker_restarts_total`、`pty_pool_managed_worker_active` ゲージ |
| 優雅なフォールバック | 全 PTY パスはエラー時にレガシー `tokio::process::Command + claude -p` へフォールバック — worker 欠如 / pool 不健全 / spawn 失敗は回復可能 |

## MCP HTTP/SSE トランスポート（W20）

| 機能 | 説明 |
|------|------|
| HTTP Server | `duduclaw http-server --bind 127.0.0.1:8765` — Bearer 認証 REST + SSE |
| エンドポイント | `POST /mcp/v1/call`（単一 JSON-RPC ツール呼出）、`GET /mcp/v1/stream`（長命 SSE）、`POST /mcp/v1/stream/call`（非同期 + SSE push）、`GET /healthz`（認証なし）|
| レート制限 | Token bucket `OpType::HttpRequest`、60 req/min |
| SSE 接続ストア | `mcp_sse_store.rs` が broadcast channel で SSE 接続を管理 |

## ERP 連携

| 機能 | 説明 |
|------|------|
| Odoo Bridge | 15 MCP ツール（CRM/販売/在庫/会計）、JSON-RPC ミドルウェア |
| Edition Gate | CE/EE 自動検出、機能ゲート |
| イベントポーリング | Odoo 状態変化をエージェントに能動通知 |
| エージェント別認証分離 | `OdooConnectorPool` を `(agent_id, profile)` でキー化、監査ログに `profile` + `ok=bool` を付与（v1.11.0 / RFC-21 §2）|
| ダッシュボード保存前テスト | `odoo.test` が inline params を受理。認証情報を省略すると保存済みの値にフォールバック。inline モードでも同じ SSRF / HTTPS / db-name バリデータを適用（v1.13.1）|

## RL と可観測性

| 機能 | 説明 |
|------|------|
| RL Trajectory Collector | チャネル対話中に `~/.duduclaw/rl_trajectories.jsonl` へ書込 |
| `duduclaw rl` CLI | `export` / `stats` / `reward` — 複合報酬（outcome × 0.7 + efficiency × 0.2 + overlong × 0.1） |
| Prometheus メトリクス | `GET /metrics` — requests / tokens / duration histogram / channel status |
| Dashboard WS ハートビート | サーバー Ping 30s + 60s アイドルクローズ；クライアント `ping` RPC 25s |
| BroadcastLayer | tracing レイヤー、リアルタイムログを WebSocket 購読者にストリーミング |

## メモリ評価と Python レイヤー

| 機能 | 説明 |
|------|------|
| LOCOMO メモリ評価（W21、v1.9.4）| `python/duduclaw/memory_eval/` — `retrieval_accuracy` / `retention_rate` / `locomo_integrity_check`；`cron_runner` 毎日 03:00 UTC；5 分 `smoke_test` P0；`build_golden_qa.py` がゴールデン QA セットを構築；200 件 `data/golden_qa_set.jsonl`；`duduclaw-memory` バッチクエリ API |
| Python Agents ルーティング（v1.9.4）| `python/duduclaw/agents/` — 能力ベースルーティング（`capabilities/` manifest loader + matcher、`routing/` router + resolution + memory_resolver）|
| Python MCP スコープ強制（v1.9.4）| `python/duduclaw/mcp/` — API key 認証 + key masking；memory ツール（store/read/search/namespace/quota）を `execute()` 入口で厳格にスコープ強制（`memory:write` / `memory:read`）|

## Web ダッシュボード

| 機能 | 説明 |
|------|------|
| 23 ページ | Dashboard / Agents / Channels / Accounts / Memory / Security / Settings / OrgChart / SkillMarket / Logs / WebChat / OnboardWizard / Billing / License / Report / PartnerPortal / Marketplace / KnowledgeHub / Odoo / Login / Users / Analytics / Export |
| 技術スタック | React 19 + TypeScript + Tailwind CSS 4 + Base UI + CVA |
| DuDuClaw デザインシステム（mds）| 共有 `web/src/components/mds/` コンポーネントライブラリ（OKLCH トークン、4 層サーフェス、3 段シャドウ、Inter／Geist Mono）+ `nav-model.ts` のグループ化サイドバー（個人／作業／会社／設定）+ `web/DESIGN.md` 仕様；全ページを共有プリミティブで構築、en/ja/zh i18n 同期 |
| リアルタイムログストリーミング | BroadcastLayer tracing → WebSocket push |
| Memory Key Insights | `key_facts` カード + access_count バッジ + タイムスタンプ + メタデータ |
| Memory Evolution | SOUL.md バージョン履歴 + 前後メトリクス差分 + 状態バッジ |
| Logs 履歴リライト | ソースフィルタチップ + ソース別カウント + 重要度ドロップダウン + 重要度着色左枠 + JSON 詳細展開 |
| Toast 通知 | モジュールスコープイベントバス、max-5 キュー、暖色 stone/amber/emerald/rose バリアント |
| OrgChart | D3.js インタラクティブエージェント階層 |
| Session Replay | 会話再生コンポーネント + タイムライン |
| WikiGraph | インタラクティブ知識グラフ |
| 国際化 | zh-TW / en / ja-JP（600+ 翻訳キー） |
| ダーク/ライトテーマ | システム設定 + 手動トグル |
| Experiment Logger | RL/RLHF オフライン分析用のトラジェクトリ記録 |
| Marketplace RPC | `marketplace.list` が実 MCP カタログを提供（Playwright / Browserbase / Filesystem / GitHub / Slack / Postgres / SQLite / Memory / Fetch / Brave Search） |
| Partner Portal | SQLite `PartnerStore` + 7 RPCs（profile/stats/customers CRUD） |

## 商用機能

| 機能 | 説明 |
|------|------|
| ライセンスティア | Free / Pro / Enterprise |
| ハードウェアフィンガープリント | ライセンスバインディング |
| 業種テンプレート | 製造業 / 飲食業 / 貿易業 |
| CLI ツール | 12+ サブコマンド |
| Partner Portal | マルチテナント販売代理店インターフェース |
