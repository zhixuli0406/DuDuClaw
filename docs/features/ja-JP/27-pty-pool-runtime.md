# クロスプラットフォーム PTY Pool + Worker

> AnthropicがOAuthアカウントの`claude -p`をブロックしたとき、私たちは手紙を送るのをやめ、代わりに電話回線を開いたままにしました。

---

## たとえ話：手紙 vs. 開いたままの電話回線

これまでClaude CLIと会話する方法は、**質問のたびに手紙を送る**ようなものでした。`claude -p "あなたのプロンプト"`と書き、封筒を閉じ、送り出すと、真新しい配達員（まったく新しいプロセス）が運びます。各手紙は完結した独立した往復でした。シンプルです——郵便局が配達をやめるまでは。

2026年半ば、AnthropicはOAuthサブスクリプションアカウントの`claude -p`をブロックしました。手紙は差し戻されました。

解決策は、**同僚との電話回線を開いたままにして、リアルタイムで会話する**ことです。質問のたびに手紙を送る代わりに、一度ダイヤルし、回線は接続されたまま、同じ通話の中で質問を次々と話します。その同僚とは、本物の対話型`claude` REPL——人間が手で操作するのと同じプログラムです。

しかし電話通話には手紙にはない問題があります。相手が**話し終えた**のか、ただ間を置いているだけなのか、どう判断するのか。そこで**合言葉**を決めます。「どうぞ」。「どうぞ」と聞こえたら、そのターンが完了し、自分の番だとわかります。DuDuClawはこの合言葉を **sentinel** と呼びます——モデルがすべての回答を包む目印で、runtimeが応答の開始と終了を正確に把握できるようにします。推測も、会話履歴全体を聞いて答えを探すことも不要です。

これがPTY Poolの核心です。本物のターミナル（開いた回線）、sentinelプロトコル（合言葉）、そして事前にウォームアップされたsessionのプール（既に保留中で、いつでも話せる同僚たち）。

---

## なぜスクロールバックの掻き取りではなく本物のPTYか

対話型REPLをプログラムで駆動するには、2つの素朴な失敗モードがあります：

1. **通常のsubprocessのようにpipeする** ——しかし`claude`は本物のターミナルに接続されていないことを検知し、対話モードでの実行を拒否します。多くのCLIがこうです。
2. **スクロールバック画面を掻き取る** ——ターミナルが出力するすべてを取り込み、ノイズ（バナー、スピナー、ANSIカラーコード、プロンプト装飾）から答えを解析しようとします。脆く、遅い。

DuDuClawはどちらも採用しません。**本物の疑似ターミナル（PTY）**を割り当て、`claude`に人間がタイプしていると信じ込ませ、次に**インバンドのsentinel framingプロトコル**を使い、答えが境界を事前に区切られた状態で届くようにします——スクロールバックの掻き取りなし、サイドカープロセスなし。

```
   素朴なアプローチ                   PTY Pool アプローチ
   ────────────                      ─────────────────
   claude（pipeを拒否）              本物のPTY（claudeはTTYを見る）
        │                                  │
   スクロールバック掻き取り           read_until(sentinel)
        │                                  │
   ANSIノイズから正規表現抽出        payloadは事前framing済みで届く
        │                                  │
   ❌ 脆い                            ✅ 決定的
```

PTYバックエンドはクロスプラットフォーム——これこそが単一のコードパスでmacOS、Linux、Windowsをカバーできる鍵です：

| プラットフォーム | PTYバックエンド | 提供元 |
|------------------|-----------------|--------|
| Windows 10 (1809+) / 11 | ConPTY | `portable-pty` |
| macOS | openpty | `portable-pty` |
| Linux | openpty | `portable-pty` |

先行する事例（tmux経由の`dorkitude/maude`、Unixドメインソケットの PTY supervisor経由の`runtorque/torque`）はいずれも**Unix専用**でした。`portable-pty`こそが、単一のruntimeで3つのOSすべてをカバーできるようにするピースです。

---

## Sentinelプロトコル

`PtySession`がspawnすると、`--append-system-prompt`命令を注入し、モデルにsentinel包囲プロトコルを教えます：sentinel行を1つ出力し、続いて答えのテキスト、そしてまったく同じsentinel行をもう一度——閉じsentinelの後には何も出力しない。

```
Gateway                          PTY                         claude REPL
   │                              │                              │
   │  invoke("proxy設定は         │                              │
   │          何？")             │                              │
   ├─────────────────────────────►  プロンプトをPTYに書き込み ─►│
   │                              │                              │ 思考中…
   │                              │  ◄─── <SENTINEL> ────────────┤
   │                              │  ◄─── 答えのテキスト ────────┤
   │                              │  ◄─── <SENTINEL> ────────────┤
   │  read_until(閉じ             │                              │
   │            sentinel)        │                              │
   │  ◄── sentinelペア間の ───────┤                              │
   │      payload                │                              │
```

Runtimeは閉じsentinelが見えるまで読み込み、**sentinelペアの間**のpayloadを抽出します。閉じsentinelがread-untilのプローブなので、runtimeは周囲のターミナル装飾を解釈する必要が一切ありません——framingされた答えを切り出すだけです。実装は意図的に**最後のペア**のsentinel出現箇所を取り、ターミナルが開きsentinelをassistant装飾とインラインでレンダリングするケースにも耐えます。

長寿命sessionが不要な場合のために、別の **one-shot** パス（`oneshot_pty_invoke`）も存在します。これも本物のPTYを通して実行されますが（CLIはTTYを見る）、sentinel framingは**注入しません**——従来の単発呼び出しのライフサイクルを踏襲します。

---

## RuntimeMode：2つのルート、1つのデフォルト

各agentの返信は、その`agent.toml`から選ばれた`RuntimeMode`によってルーティングされます。この機能は**デフォルトOFF**——agentごとにオプトインします。

| RuntimeMode | パス | 使用条件 |
|-------------|------|----------|
| `FreshSpawn` | `call_claude_cli_rotated`経由の旧来の`tokio::process::Command` | デフォルト；`agent.toml`が欠落、不正、またはフラグ未設定のとき |
| `PtyPool` | 本crateのプール化された、sentinel framingのPTY session | `[runtime] pty_pool_enabled = true`のときのみ |

`runtime_mode_for_agent()`はagentディレクトリを読み、**フェイルセーフで`FreshSpawn`に戻ります**——ファイル欠落、解析エラー、フラグ未設定はすべて旧来のパスをデフォルトとします。Gatewayの公開インターフェースは`acquire_and_invoke` / `acquire_and_invoke_with`で、プールからsessionを取り出し、sentinel往復を1回実行して返します。

```
agent X のchannel reply
        │
        ▼
runtime_mode_for_agent(agent_dir)
        │
   ┌────┴─────────────────────────┐
   │                              │
FreshSpawn                     PtyPool
   │                              │
tokio::process::Command        acquire_and_invoke()
claude -p（旧来）              プール化sentinel session
```

---

## OAuth vs. API-Key ルーティング

`PtyPool`ブランチ内では、`channel_reply`はアカウント種別で分岐します——`claude -p`のブロックはOAuthサブスクリプションアカウントにのみ当たったからです：

| アカウント種別 | ルート | 理由 |
|----------------|--------|------|
| OAuthサブスクリプション | 長寿命対話型REPL（sentinel framing） | これらの`claude -p`はブロック済み；REPLが唯一のパス |
| API key | `oneshot_pty_invoke` + `claude -p` | `-p`はAPI-key認証では依然有効；sessionを保持する必要なし |

こうしてOAuthアカウントは開いた電話回線を得て、API-keyアカウントは手紙を送り続けます——どちらも本物のPTYを通してです。`claude_runner` dispatcherは同じショートサーキットを適用し、サブagentのディスパッチとchannel replyを一貫させます：`pty_pool_enabled = true`のとき、両者ともlocal-offloadとhybrid routingをスキップします。

---

## Phase 7：管理されたWorker

より強い隔離のために、プールは別の`duduclaw-cli-worker`サブプロセスとして**プロセス外**に存在させることができ、`[runtime] worker_managed = true`で制御されます。Gatewayの`worker_supervisor`がそのライフサイクルを所有し——決定的に重要なのは、その終了をgatewayのグレースフルシャットダウンのフローに順序づけて組み込むことです：

```
Gateway グレースフルシャットダウン
        │
        ▼
予測エンジンをflush
        │
        ▼
worker_supervisor: SIGTERM ──► duduclaw-cli-worker
        │  （待機）                  │ in-flightを排出
        ▼                            │
worker_supervisor: SIGKILL ──► （まだ生存していれば）
        │
        ▼
axum がHTTP接続を排出
```

Workerは予測エンジンのflushの**後**、axumの排出の**前**に終了されます——だから作業は失われず、gateway終了後にゾンビプロセスが残ることもありません。

---

## Fallbackチェーン：致命的ではなく復旧可能

runtime全体で最も重要な性質：**すべてのPTYパスはエラー時に旧来の`tokio::process::Command + claude -p`にフォールバックします。** worker欠落、プール不健全、spawn失敗は復旧可能——致命的ではありません。

```
acquire_and_invoke()
     │
     ├─ プール健全？     ──否──► 旧来のspawnにフォールバック ──┐
     │                                                       │
     ├─ session spawn成功？否─► 旧来のspawnにフォールバック ──┤
     │                                                       │
     ├─ sentinel到着？    否──► 旧来のspawnにフォールバック ──┤
     │                                                       │
     ▼ 是                                                    ▼
   framing済みpayloadを返す                          claude -p の結果
```

これは`pty_pool_enabled`をオンにしても、agentが旧来のパスより**信頼性が下がる**ことは決してないことを意味します。最悪の場合でも、以前とまったく同じものへ静かに劣化するだけです。

---

## Phase 8.5：Runtime ステータスエンドポイント

`runtime_status.rs`は`GET /api/runtime/status`を公開します——**loopback専用**のJSONエンドポイント（非loopbackのピアは403；loopback境界**そのもの**が認証です）。ライブのプールカウンターと、グローバルkill switchが有効かどうかを報告します。

```
$ curl http://127.0.0.1:<port>/api/runtime/status
{
  "kill_switch_active": false,
  "pool": {
    "acquires_cache_hit_total": 412,
    "acquires_spawn_total": 9,
    "evicted_idle_total": 3,
    "evicted_unhealthy_total": 0,
    "invokes_ok_total": 421,
    "invokes_empty_total": 0
  }
}
```

---

## Phase 8：Prometheus 可観測性

このruntimeは`pty_pool_*`カウンター群とworkerヘルスゲージをエクスポートし、キャッシュ効率とfailover挙動を観察できるようにします：

| メトリクス | 意味 |
|------------|------|
| `pty_pool_acquires_cache_hit_total` | プールから再利用されたsession（ウォーム） |
| `pty_pool_acquires_spawn_total` | 新規spawnされたsession（コールド） |
| `pty_pool_evicted_idle_total` / `_unhealthy_total` / `_shutdown_total` | 3つの追い出し理由 |
| `pty_pool_invokes_ok_total` / `_empty_total` | invoke結果 |
| `pty_pool_invoke_duration_*` | 往復時間のhistogram |
| `worker_health_misses_total` / `worker_restarts_total` | 管理worker のヘルス |
| `pty_pool_managed_worker_active` | モードゲージ（worker オン/オフ） |

`cache_hit`対`spawn`の比率が高いほど、プールが役割を果たしている証拠です——ほとんどのターンがコールドスタートのコストを払わず、ウォームsessionを再利用しています。

---

## 設定

すべて`agent.toml`内でagentごとに設定し、デフォルトはオフです：

```toml
[runtime]
pty_pool_enabled = true   # 対話型PTY poolにオプトイン（デフォルトfalse）
worker_managed   = true   # プールをプロセス外のduduclaw-cli-workerで実行
```

両方とも未設定の場合、agentは以前とまったく同じ`FreshSpawn`旧来パスで動作します。`pty_pool_enabled`のみ設定するとプールはプロセス内で動作し、`worker_managed`を追加すると監視されたサブプロセスへ移動します。

---

## なぜ重要か

### OAuthアカウントを解放する

2026年半ばにAnthropicがOAuthサブスクリプションの`claude -p`をブロックしたとき、Pro/Team/Maxアカウントに支えられたすべてのchannel replyが失敗していたはずです。対話型REPLパスは、人間が`claude`を駆動するのと同じ方法でこれらのアカウントを復活させます——いかなるポリシーも回避せず、ただプログラムを期待される実行方法どおりに実行するだけです。

### 1つのコードパス、3つのOS

`portable-pty`（WindowsのConPTY、Unixのopenpty）により、同じruntimeがmacOS、Linux、Windowsで動作します。借用した先行事例はUnix専用でした；こちらはクロスプラットフォーム版です。

### 決定的な解析

sentinelプロトコルにより、runtimeは答えがどこで終わるかを推測する必要がありません。スクロールバックの掻き取りも、脆いANSI正規表現もなし——答えは2つの目印の間にframingされた状態で届きます。

### オンにしても安全

デフォルトオフ、`FreshSpawn`へフェイルセーフ、そしてすべてのPTYエラーは旧来の`claude -p`パスへ劣化します。プールを有効にしても、旧来の挙動と同等以上にしかなりません——正常に動いていたagentを壊すことはありません。

### 可観測

loopbackステータスエンドポイントと`pty_pool_*` Prometheusメトリクスにより、プールのウォーム度、追い出し、workerヘルスが可視化され、プールが実際にコールドスタートを節約しているか確認できます。

---

## まとめ

Anthropicは「質問のたびに手紙を送る」能力を取り上げました。DuDuClawの答えは、電話回線を開いたままにすることでした——`claude`に人間がキーボードの前にいると思わせる本物のPTY、runtimeが各回答の終わりを正確に把握するためのsentinel合言葉、そしてほとんどのターンがコールドスタートをスキップできるウォームsessionのプール。単一のコードパスでmacOS、Linux、Windowsをまたぎ、デフォルトはオフ、そしてすべての失敗は以前うまく動いていた方式へ静かにフォールバックします。郵便局はルールを変えました；しかし会話は続いたのです。
