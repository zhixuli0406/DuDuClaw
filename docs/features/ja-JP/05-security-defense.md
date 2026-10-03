# セキュリティ防御

> 稼働中の4つのガード、それぞれがどこで動くか、そしてどれもカバーしないもの。

---

## 経緯についての注記

2026-09まで、このページは3段階のシェルスクリプト防御を説明していました：決定的ブラックリスト、難読化／流出スキャナー、Haiku AI判定。いずれも `.claude/hooks/` に置かれ、GREEN／YELLOW／RED の脅威レベルステートマシンが制御するという内容でした。

それらのスクリプトは、`.claude/` を公開リポジトリから外したコミット `ba015a48` で削除済みです。現在 `.claude/` は丸ごと gitignore されています。出荷バイナリのどこからも読まれていませんし、脅威レベルステートマシンも存在しません。

実際に製品にあるものは、より小さく、より推論しやすいものです：gateway が各エージェントディレクトリにインストールする **2つの PreToolUse フック**（どちらも Rust のサブコマンド）、メッセージ経路上の **1つの入力スキャナー**、そして「誰が誰に指示できるか」を決めるファイル群に対する **1つのフィールド単位の凍結**。

---

## ガード1 — `agent-file-guard`（PreToolUse、Rust）

`duduclaw hook agent-file-guard` はシェルスクリプトではなく実際のサブコマンドなので、macOS／Linux／Windows で同一に動作します。gateway は matcher `Write|Edit|MultiEdit|Bash` で `<agent_dir>/.claude/settings.json` に登録し、起動のたびに再登録します（`agent_hook_installer`）。登録は運用者の既存設定を上書きせずマージする方式です。

次の場合に exit 2（Claude Code は「このツール呼び出しをブロック」と解釈）を返します：

- エージェントが**エージェント構造ファイル**（`agent.toml`、`SOUL.md`、`CLAUDE.md`、`.mcp.json` …）を正規の `<home>/agents/<name>/` ツリー**外**に書き込んだとき。新規エージェントの作成は `create_agent` MCP ツール経由に限られ、そちらには委譲認可ゲートがあります；
- エージェントが**自分自身の `SOUL.md`** を書き込んだとき。場所が正しくてもブロックします。人格は運用者が管理します。このフックにオプトインはありません。`agent.toml [permissions] can_modify_own_soul = true` で明示的にオプトインしたエージェントは、`agent_update_soul` MCP ツールを通じて自分の `SOUL.md` を変更でき、ファイルを直接書くことはできません；
- エージェントが**自分自身の `CONTRACT.toml`** を書き込んだとき。場所が正しくてもブロックします（判定 `BlockedOwnContractWrite`）。契約は運用者がエージェントに課す境界なので、オプトインのフラグはありません。ブロック時のメッセージは運用者に依頼するよう伝え、運用者はダッシュボードから変更します（`contract.update`、管理者のみ。このフックは通りません）；
- エージェントが**他のエージェント**のファイルに触れたとき；
- エージェントが、削除された社員が保管される **`agents/_trash/`** 配下に何かを書き込み、移動、または削除したとき（Bash はヒューリスティック）；
- エージェントが Bash から **`duduclaw agent create <name>`** を、削除された社員が使っていたために予約されている名前で実行したとき（[委譲の隔離](37-delegation-isolation.md#削除された社員の名前は予約されたままになる)を参照）。この拒否は `agent_name_reserved` として、`path_kind` を `cli_bash_agent_create` として監査されます。

Bash では、自分の `SOUL.md` と自分の `CONTRACT.toml` のルールはヒューリスティックです。書き込み形のコマンドがファイルを `agents/<自分>/…` として、または `CONTRACT.toml`、`./CONTRACT.toml` のような相対表記で指定するとブロックされます。これは減速帯にすぎません。ファイル名を隠すコマンド（変数、エンコードした文字列、スクリプト）はすり抜けられます。本当の隔離は、エージェントに Bash を与えないことです。

ライブフォーク（`fork_run`）は同じファイルを反対側から守ります。ブランチはエージェントの構造ファイルを読めますが、ブランチをエージェントのディレクトリに昇格するとき、`SOUL.md`、`CONTRACT.toml`、`agent.toml`、`.mcp.json`、`.claude/` などのエージェント構造ファイルが親のものを上書きすることはありません。

## ガード2 — `data-file-guard`（PreToolUse、Rust、RFC-23 §14.4）

ガード1が守るのは DuDuClaw 自身の構造ファイル、こちらが守るのは顧客のデータです。

`Read` と `Bash` は Claude Code の組み込みツールなので、`cat customers.csv` は `file_read`／`csv_read`／`xlsx_read` が必ず通る MCP 匿名化チョークポイントを通りません。インストーラーは `duduclaw hook data-file-guard` を matcher `Read|Bash` に登録します。判定ロジックは `duduclaw_core::data_file_guard` にあり、CLI サブコマンドと gateway のインストーラーテストが同じ実装を共有します。契約はガード1と同じ：exit 0 で許可、exit 2 ＋ stderr でブロック、stderr はモデルに提示されます。

gateway が spawn 時に `DUDUCLAW_DATA_FILE_GUARD` を設定しない限り何もしません。gateway がこれを設定するのは、そのエージェントで匿名化が実際に有効なときだけです。匿名化がオフの環境では、このガードが存在する前とバイト単位で同じ挙動になります。

H10（2026-09）以前は `<agent_dir>/.claude/hooks/data-file-guard.sh` に置かれた POSIX シェルスクリプトで、`PATH` に bash がない Windows ホストでは**まったく機能していませんでした**——フックコマンドが失敗し、Claude Code は 2 以外の終了コード（"command not found" を含む）を*許可*として扱うため、誰も気づかない場所でガードが消えていたのです。インストーラーはアップグレード時に残存スクリプトを削除するので、古いコピーが現役のガードと取り違えられることはありません。

**明示された制約**：`Bash` 側の検査はファイル名を照合します。パスを動的に組み立てるコマンド（`python -c "open(chr(99)+…)"`）はそのまま通り抜けます。本当の防御は MCP ツール面であり、このガードはモデルが無防備な経路を取る確率を下げるものです。ヒューリスティックであってサンドボックスではありません。

## ガード3 — `input_guard`（プロンプトインジェクションスキャナー、Rust ライブラリ）

`duduclaw_security::input_guard::scan_input` はテキストを**11のルールカテゴリ**で 0–100 点に採点し、`DEFAULT_BLOCK_THRESHOLD`（60）以上でブロックします：

| ルール | 重み | 単独即ブロック |
|---|---|---|
| `instruction_override` | 40 | はい |
| `role_hijack` | 35 | はい |
| `tool_abuse` | 30 | はい |
| `data_exfiltration` | 25 | はい |
| `system_prompt_extraction` | 30 | いいえ |
| `encoding_bypass` | 25 | いいえ |
| `termination_manipulation` | 30 | いいえ |
| `authority_escalation` | 信号1種類につき 35、異なる2種類で加算 | いいえ |
| `memory_poisoning` | 信号1種類につき 30、異なる2種類で加算 | いいえ |
| `role_provenance` | 枠1種類につき 35、異なる2種類で加算 | いいえ |
| `action_binding` | 30 | いいえ |

パターンは英語と中国語（繁体字と簡体字）をカバーします。テキストは先に NFKC 正規化され（`unicode_normalizer`）、ホモグラフや不可視文字の小細工はパターン照合をすり抜けられません。

**中国語のカバー範囲（v1.67.1）。** リリース済みのバージョンは中国語の指示上書きを4つの完全一致文字列でしか照合しておらず、「所有」「之前」「的」などの語を1つ挟むだけで通り抜けました。実地テストでは、そうした4文が `user_profile_record` 経由で保存されました。v1.67.1 からは次のとおりです。

- `instruction_override`：上書き動詞（忽略／無視／忘記／忘掉／不要理會／不用理會／別管、簡体字形を含む）の後、同じ節の中で 12 文字以内に指示系の名詞（指示／指令／規則／提示詞／系統提示）があり、その間に範囲語（先前／之前／以上／上面／上述／前面／所有／全部／一切／你的／原本／原來）がある場合に一致します。空白は数えず、`。！？；` と改行で節が終わります。重みと即時ブロックは英語のフレーズと同じです。
- `system_prompt_extraction`：抽出名詞（系統提示詞／系統提示語／你的系統提示／你的指示／你的設定）と出力動詞（輸出／顯示／告訴我／給我看／洩漏／列出／重複）が順不同で 12 文字以内にある場合。採点は英語のルールと同じで、重み 30、単独ではブロックしません。単独の「系統提示」は「システム通知」の意味もあるため抽出名詞に含めていません。
- `role_hijack`：「你現在是管理員模式」「開發者模式」「越獄模式」「你現在不受限制」「進入越獄模式」などの固定フレーズと、単独の「越獄模式」。英語と同じ扱いです。
- しきい値と英語のリストは変わりません。

**既知の誤検知。** 形で照合するため、上書き動詞・範囲語・指示系の名詞が1つの短い節に並ぶ普通の文もブロックされます。たとえば「請忽略之前寄的指示，以新版為準」「請忽略以上規則中的第三條，已經取消」「忘記之前的規則了，可以再說一次嗎」です（テスト `known_benign_shapes_that_do_match` で固定）。「越獄模式」に触れるだけでもブロックされます。回避策は上書き動詞を使わずに言い換えることで、たとえば「之前的指示作廢，以新版為準」です。近い文「之前的報價單請作廢，以新的指示為準」は、一致してはならない文のテスト一覧に入っています。このスキャナーはフレーズによるヒューリスティックで分類器ではなく、実際の会話データでの計測もしていません。

`termination_manipulation`（LoopTrap、arXiv:2605.05846）は意図的に即ブロックにしていません。重み 30 は閾値より低く、単独一致では警告と監査のみでブロックしません。これにより通常の「続けてください」が誤検知されずに済みます。

**文型ファミリー（v1.68.0）。** レッドチーム台帳で、AI 社員を狙う4つの手法がどれも0点だとわかりました。現在はそれぞれに英語と繁体字中国語のアンカー付き正規表現があります。

- `authority_escalation`：ユーザーの権限ではなく社員自身の権限で実行させようとする文（"use your own service account / admin role"、"not my permissions"、"the system trusts you more than me"、用你自己的服務帳號／管理員身分、不要用我的權限、系統比較信任你）。
- `memory_poisoning`：恒久的な特権を記憶させようとする文（"remember this permanently"、"as a fact about me"、"I am always allowed to"、永久記住、記在你的記憶裡、以後每次對話都照辦、我一律有權限）。普通の「請記住…」、"remember to…"、"remember my name" は一致しません。
- `role_provenance`：ユーザーメッセージ内の偽のシステム・ツール・運営者の枠：`[SYSTEM]`、`<tool_result>` タグ、`approval=granted`、"policy update from the platform operator"、「本次對話已解除…限制」、平台營運方政策更新、引用文中の AI 宛てメモ（給 AI 助理的附註）とそれを隠す指示（不要提到這段附註）。枠はタグ全体でのみ一致するので "our system sends a tool result" は一致しません。開始タグと終了タグの組は1種類として数えます。
- `action_binding`：1つの承認を別の操作に流用する文（"use that same approval to…"、"use the approval from step 1"、用同一個核准、用第一步的核准去…）。「既然已經核准預算，請安排會議」は一致しません。

重みの方針：信号1つだけなら警告と監査のみ（30–35、60 未満）です。1つのメッセージに同じファミリーの**異なる**信号が2種類ある場合、または信号1つに既存ルール（`instruction_override`、`system_prompt_extraction` など）が加わる場合にブロックします。自身で加算されるのは3つのファミリーです：`authority_escalation`（「用你的服務帳號」と「系統比較信任你」で 70）、`memory_poisoning`（「永久記住」と「我一律有權限」で 60）、`role_provenance`（`[SYSTEM]` と `approval=granted` で 70）。`action_binding` は自身では加算されず、他のルールと組み合わさったときだけブロックします。同じ信号が2回出ても1回と数えます。`[SYSTEM]` 1つ、または `<tool_result>…</tool_result>` の1組だけなら 35 のままです。既知の代償：たまたま信号を2種類含む普通の文もブロックされます。たとえば "please use your admin account, not my permissions, to fix the shared folder" です（テスト `known_benign_shapes_blocked_by_stacking` で固定）。信号が1つだけの言い方にするか、管理者に直接頼んでください。一致があれば本文を捨てる呼び出し側（蒸留、プロフィール書き込み）は、これらの文型を含む本文も捨てるようになりました。各ファミリーの陽性例と、似ているが一致してはならない文は `input_guard.rs` のテストで固定しています。

一致したときに影響が出る場所（確認済みの呼び出し箇所）：

- チャットで受信したメッセージ（`channel_reply`、`scan_input_with_audit`）：ブロックされたメッセージには警告の返信が返り、AI には渡されません。
- MCP ツール呼び出し（`mcp_dispatch`、シリアライズした引数に `scan_input_with_audit`）：引数がブロック対象の文を引用していると、呼び出しは拒否され監査されます。
- 会話事実・プロフィール・知識振り分けの抽出（`wiki_ingest`、`profile_distill`、`knowledge_route`）：ブロックしない抽出ルールを含め、**いずれか**のルールに一致した内容は破棄されます。
- `user_profile_record`：predicate と値をスキャンし、ブロック水準なら拒否します。
- `duduclaw migrate-from` のインポートはブロック対象の項目をスキップし、エキスパートパックのインストールはブロック対象のパックを拒否します。
- Agent Mail：一致した受信メールも保存されますがフラグが付き、フラグ付きのメールが AI 従業員を起動することはありません。
- リマインダー：プロンプトがブロック対象のリマインダーは実行されません。

## ガード4 — `org_field_guard`（組織権限の凍結）

A2A 委譲の述語（`delegation_policy::can_delegate`）は、`agent.toml` の `[agent] reports_to`／`department`／`name` と `config.toml` の `[delegation]`・`[acp]` を読んで「誰が誰に指示できるか」を決めます。どちらもただのファイルなので、`Edit` を持つエージェントは自分の `reports_to` を被害者に向けて書き換え、「部下 → 上位」ルールを主張できてしまいます。裁かれる側が証拠を所有している状態でした。

`org_field_guard` は同じ `agent-file-guard` フックの中で動き、再構成した**書き込み後**の内容をディスク上の現状とフィールド単位で比較します。保護対象のフィールドやセクションに変更があれば拒否します。`[capabilities]` はキー名リストではなく**テーブル全体**として凍結されるので、将来のリリースで追加される capability キーは、誰かがリストを拡張するのを思い出した日ではなく、着地した当日から保護されます。

構造上 fail-closed です：新しい内容がパースできない、既存の内容がパースできない、書き込み意図が再構成できない——いずれも拒否します。ファイルがまだ存在しない場合は許可します。作成は `create_agent` 経由であり、そちらに独自のゲートがあるためです。

正当な変更の経路はすべて残っています：MCP `agent_update` ツールとダッシュボードの `agents.update` RPC。どちらもこのフックを通りません。

---

## 補助レイヤー

**MCP 認可ゲート** — すべての MCP ツールはスコープ表に列挙されており、表にないツールは既定で Admin スコープを要求します。スコープ、エージェント単位の capability 付与、`denied_tools` はそれぞれディスパッチのフロントドアで強制され、拒否はすべて `error_class` 付きで監査されます。

**SOUL.md ドリフト検知** — `soul_guard` は起動時と各ハートビートティックで `SOUL.md` の SHA-256 フィンガープリントを取り、`.soul_history/` に最大10世代のバックアップを保持し、Agent Stability Index とともにドリフトを報告します。

**監査証跡** — `tool_calls.jsonl` はすべてのツール呼び出しを記録し、`result_text`／`input_text` はマスク済み（3パスのシークレットマスキング、切り詰めより先にマスク）、パーミッションは `0600`、行はハッシュチェーンで連結され、16 MB でローテーションします。`security_audit.jsonl` はセキュリティイベントを別に保持します。このログはグラウンディング事前チェックと受け入れ判定者が読む証拠源でもあるため、弱めれば検証も弱まります。

**エージェント単位の鍵分離** — MCP API キーとコネクタ認証情報はエージェント単位で、`secret_ref` 経由で解決されます。1つのエージェントの漏洩がプラットフォーム全体の漏洩にはなりません。チャネルの認証情報は2種類に分かれます。LINE、WhatsApp、Feishu、Google Chat、Teams、WeCom、DingTalk は `config.toml [channels]` にあるデプロイ全体共通の認証情報を使い、社員専用の bot トークンがあるのは Telegram、Discord、Slack だけです（自分のトークンがない社員は `reports_to` をたどって上位を探し、最後にグローバルのトークンを使います）。

**チャネル上のチャットコマンド（v1.68.0）** — `!STOP`、`!STOP ALL`、`!RESUME`、`/model <名前>` には管理者が必要です。WhatsApp、Feishu、Teams、WeCom、Google Chat、DingTalk では以前、すべての送信者に `is_admin = true` を渡していたため、bot にメッセージを送れる人なら誰でも停止や再開ができました。現在これらのチャネルは、送信者 id または会話 id をチャネルの `admin_users` 設定（グローバル範囲。Google Chat と Teams も設定可能になりました）と完全一致で照合し、一覧がなければ誰も管理者になりません。WebChat では、有効なダッシュボードアカウントで役割が管理者のものだけが該当し、Web サイト用ウィジェットの訪問者は該当しません。

**キルスイッチのしきい値（v1.68.0）** — `KILLSWITCH.toml [triggers]` の4つのしきい値には以前は読み取り側がありませんでした。現在はファイルに書かれていて範囲内のキーだけが有効になり、セキュリティ設定ページではしきい値ごとにチェックボックスがあります（チェックを外すと `null` を送り、キーを削除します）。ファイルが変わると読み直します。`cost_limit_usd` は全社員の24時間の支出と比べ、達するとグローバルの failsafe レベルを制限状態にし、failsafe が自然に回復するか誰かが `!RESUME` を送るまで続きます。`max_replies_per_minute` は会話ごとに数え、超過分は黙って破棄します。`max_consecutive_errors` と `error_rate_threshold`（直近20回、最低10回）はその会話の failsafe レベルを1段階上げます。発動ごとに `killswitch_trigger` として監査されます。`KILLSWITCH.toml` の `[audit]` セクションは読まれなくなりました。

**秘匿化のデータソース保護（v1.68.0）** — 「プライバシー / 秘匿化」タブの「資料來源保護」スイッチが機能するようになりました。`user_input` はチャネルのメッセージを AI に渡す前に、`system_prompt` は組み立て済みのプロンプトを（既定では `apply_to_system_prompt` が付いたルールだけ）、`cron_context` は条件スクリプトのトリガーメッセージを秘匿化します。エラー時は秘匿化されていない内容を送らず、そのターンを止めます。`sub_agent` スイッチは削除されました。`purge_after_expire_days` は保管庫の掃除に使われるようになりました。

**権限フラグ（v1.68.0）** — `agent.toml [permissions]` の `can_create_agents`、`can_send_cross_agent`、`can_modify_own_skills`、`can_schedule_tasks` が `false` と書かれていると、MCP のディスパッチゲートで対応するツールが拒否されます（`permission_denied` として監査）。アップグレード後の最初の起動で古いテンプレートの `false` を `true` に移行します。[ダッシュボード設定の対応表](../../guides/ja-JP/dashboard-settings.md#ai-社員の編集ページ)を参照してください。

---

## これらのガードがカバーしないもの

これを明言すること自体が防御の一部です。

- **フックが見るのは Claude Code 自身のツール呼び出しであり、MCP ツール呼び出しではありません。** MCP には独自のゲート（スコープ、付与、`denied_tools`）があります。フックは組み込みの `Write`／`Edit`／`Read`／`Bash` 面に対する2つ目の鍵です。
- **`data-file-guard` はヒューリスティックです。** `Bash` コマンドライン中のファイル名を照合するため、動的に組み立てたパスは通り抜けます。（Windows で無効になる問題は H10 の Rust サブコマンド化で解消済みです。）
- **脅威レベルステートマシンは存在しません。** `~/.duduclaw/threat_level` は、computer use オーケストレーターがポーリングする運用者制御のキルスイッチとして残っています（`RED` で停止、`YELLOW` で一時停止）が、ワークスペース内にこれを書くものはもうありません。ファイルが無い／読めない場合は `GREEN` 扱いです。
- *（2026-09に削除。）* 本節はかつてPTYセッションプールが匿名化リライトの対象外であることを注記していました。そのプールはもう存在せず、Claudeのspawnはすべて呼び出しごとのspawn——まさにリライトがフックする形です。

---

## 他システムとの連携

- **CONTRACT.toml** はエージェントが絶対にしてはならないことを定義し、`duduclaw test` がそれをレッドチームします。ガードはツール呼び出しレベルで強制します。
- **進化エンジン** — `SOUL.md` はエージェントにとって読み取り専用なので、進化する成果物は playbook です。[38-aee-playbook-evolution.md](38-aee-playbook-evolution.md) を参照。
- **匿名化とデータソース** — `data-file-guard` が補完するパイプラインは [55-data-sources.md](55-data-sources.md) を参照。
- **委譲分離** — `org_field_guard` が守る述語は [37-delegation-isolation.md](37-delegation-isolation.md) を参照。

---

## まとめ

失効モードを明示した4つのガードは、もう裏にコードのない3層の物語に勝ります。防御を取り除いたらドキュメントも一緒に取り除かねばなりません。存在しないシェルスクリプトを説明するページは、ページが無いより悪いのです——運用者がそこで探すのをやめてしまうからです。
