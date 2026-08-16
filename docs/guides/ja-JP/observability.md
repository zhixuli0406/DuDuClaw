# 可観測性：OpenTelemetry GenAI トレーシング

DuDuClaw は agent の各ターン、モデル呼び出し、MCP ツールディスパッチのたびに [OpenTelemetry GenAI セマンティック規約](https://opentelemetry.io/docs/specs/semconv/gen-ai/) に準拠した span を発行し、OTLP 経由で対応する任意のバックエンド（Grafana Tempo、Jaeger、Langfuse、Honeycomb、Datadog など）へエクスポートできます。

独立した二つのスイッチがあります。

| スイッチ | 制御する内容 |
|---|---|
| Build feature `otel` | OTLP エクスポートパイプラインをコンパイルする。**デフォルトは OFF**：`--features otel` を付けてビルドしない限り、release binary は OpenTelemetry 依存を一切含まない。 |
| `config.toml [telemetry] otlp_endpoint`（または `OTEL_EXPORTER_OTLP_ENDPOINT` 環境変数） | 実行時にエクスポートを有効化する。未設定の場合、`otel` build であってもパイプラインは休止したまま。 |

span 自体は普通の `tracing` span であり、常に存在します（dashboard の log stream にも供給されます）。この feature が制御するのはエクスポートの有無だけです。

## ビルド

```bash
cargo build --release -p duduclaw-cli --features otel
# gateway のみ：
cargo check -p duduclaw-gateway --features otel
```

## 設定

`~/.duduclaw/config.toml`：

```toml
[telemetry]
# OTLP gRPC collector のエンドポイント（エクスポートを有効にするために必要な唯一のキー）。
otlp_endpoint = "http://127.0.0.1:4317"
# 任意設定の resource service.name（デフォルトは "duduclaw"）。
service_name = "duduclaw"
# 任意設定の head-sampling 比率、範囲は [0.0, 1.0]（デフォルトは 1.0 で全件保持）。
sample_ratio = 1.0
# 任意設定の認証ヘッダで、エクスポートリクエストごとに送信される（gRPC metadata）。
# これにより DuDuClaw は relay collector なしで、認証が必要な OTLP バックエンドと
# 直接やり取りできる。キーは小文字に正規化され、不正なエントリは警告を出して
# スキップされる（fail-safe、起動をブロックすることはない）。
otlp_headers = { authorization = "Basic <base64(user:token)>", "x-api-key" = "yyy" }
```

- **トランスポートは OTLP/gRPC**（tonic、`https` collector へは webpki roots で TLS）。`otlp_protocol = "http/protobuf"` は将来互換のためにパースされるが、現状は警告を出して gRPC にフォールバックする。
- 環境変数による上書き（いずれも標準的な OTLP の慣例）：
  - `OTEL_EXPORTER_OTLP_ENDPOINT` は `otlp_endpoint` より優先される（そのまま置き換えることもできる）。
  - `OTEL_EXPORTER_OTLP_HEADERS`：カンマ区切りの `key=value` の組で、`otlp_headers` に**上書き**マージされる（同じキーは環境変数が勝つ）。値は percent-encode されていてもよい（例：`Authorization=Basic%20<base64>`）。認証情報を `config.toml` の外に置きたいときに便利。
- ヘッダの有効性：キーは `[a-z0-9_.-]` から成る小文字化可能な ASCII でなければならず、gRPC 予約語の `-bin` サフィックスは使えない。値は可視 ASCII でなければならない。それ以外は stderr に警告を出してスキップされる。不正なヘッダが panic を起こしたり、残りのエクスポートを中断させたりすることはない。
- Fail-safe：exporter の初期化に失敗した場合は stderr に警告を出してエクスポートを無効化するだけで、gateway の起動をブロックすることはない。

> **ログレベルが重要です。** GenAI span は INFO レベルであり、グローバルな
> ログフィルタ（デフォルトは `warn`）に従います。telemetry を有効にする
> ときは、`config.toml` に `[general] log_level = "info"` を設定するか
> （あるいは `RUST_LOG=info` で実行するか）してください。さもないと span
> は一つもエクスポートされません。

## 発行される span

属性キーは `crates/duduclaw-gateway/src/otel.rs`（`attrs` module）に集約されている。GenAI semconv はまだ「Development」ステータスなので、仕様の名称変更があってもこの一ファイルを直せば済む。旧来の `gen_ai.system` とその後継である `gen_ai.provider.name` は両方とも発行される。

| Span | 発生箇所 | Attributes |
|---|---|---|
| `invoke_agent` | Channel-reply の入口（`channel_reply::build_reply_with_session_inner`）と dispatcher の agent 実行（`claude_runner::call_claude_for_agent_with_type`） | `gen_ai.operation.name=invoke_agent`、`gen_ai.system` / `gen_ai.provider.name`、`gen_ai.agent.name`、`gen_ai.request.model`、`gen_ai.usage.input_tokens`、`gen_ai.usage.output_tokens`（usage は CLI の `result` イベントから事後的に記録される） |
| `chat` | Multi-runtime のチョークポイント（`runtime_dispatch::run_agent_prompt`）と Direct API 呼び出し（`claude_runner::try_direct_api` / `try_llm_provider_api`） | `gen_ai.operation.name=chat`、`gen_ai.system` / `gen_ai.provider.name`（failover 後に実際に応答した provider）、`gen_ai.agent.name`、`gen_ai.request.model`、`gen_ai.usage.input_tokens`、`gen_ai.usage.output_tokens` |
| `execute_tool` | MCP ツールディスパッチ（`duduclaw-cli::mcp_dispatch::dispatch_tool_call`；stdio／HTTP／SSE の全 transport が対象） | `gen_ai.operation.name=execute_tool`、`gen_ai.tool.name`、`gen_ai.tool.outcome`（`ok`/`error`）、失敗時の `error.type`（scope／rate-limit／whitelist 拒否、または JSON-RPC エラー結果） |

`chat` span は稼働中の `invoke_agent` span の下にネストされるため、バックエンド側では agent の 1 ターンにつき 1 本の trace として表示され、モデル呼び出しはその子ノードになる。

## バックエンド例

### ローカルですぐ試す（Jaeger all-in-one）

```bash
docker run --rm -p 16686:16686 -p 4317:4317 jaegertracing/all-in-one:latest
# config.toml → otlp_endpoint = "http://127.0.0.1:4317" にして、http://localhost:16686 を開く
```

### 認証付き OTLP/gRPC バックエンド：collector なしで直結

`otlp_headers` を使えば、DuDuClaw は認証ヘッダ付き OTLP/gRPC を受け付ける任意のバックエンド（Honeycomb、Dash0、認証 proxy の背後にある自前ホストの Tempo/Mimir など）へ**直接**エクスポートできる。これらに対してはローカルの OTel Collector relay は任意である：

```toml
[telemetry]
otlp_endpoint = "https://api.honeycomb.io:443"
otlp_headers = { "x-honeycomb-team" = "<api-key>" }
```

あるいは Basic 認証（basic-auth の背後にある自前ホストの Grafana Tempo、gRPC 対応 gateway）：

```toml
[telemetry]
otlp_endpoint = "https://tempo.example.com:4317"
otlp_headers = { authorization = "Basic <base64(user:token)>" }
```

認証情報は環境変数で渡すほうが好ましい（config にマージされ、キーごとに環境変数が勝つ）：

```bash
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic%20<base64(user:token)>"
```

### Grafana（ローカル Tempo／Alloy）

collector の OTLP gRPC ポートを指定する：

```toml
[telemetry]
otlp_endpoint = "http://127.0.0.1:4317"
```

ローカルの Alloy／Collector は Grafana Cloud へ relay することもできる。`otlp_headers` があれば認証情報自体はそこで注入できるが、それでも relay がトランスポートを変換する役割は変わらない。Grafana Cloud のマネージド OTLP gateway が受け付けるのは **OTLP/HTTP のみ**であり（DuDuClaw の exporter は OTLP/gRPC を話す）、変換がどうしても必要になるからだ：

```yaml
# otel-collector.yaml
receivers:
  otlp:
    protocols:
      grpc: { endpoint: 0.0.0.0:4317 }
exporters:
  otlphttp/grafana:
    endpoint: https://otlp-gateway-<region>.grafana.net/otlp
    headers: { Authorization: "Basic <base64(instanceID:token)>" }
service:
  pipelines:
    traces: { receivers: [otlp], exporters: [otlphttp/grafana] }
```

### Langfuse

Langfuse Cloud の OTLP エンドポイント（`/api/public/otel`）も同様に **OTLP/HTTP のみ**を受け付けるため、最小限の gRPC→HTTP relay を残しておく必要がある（認証情報はどちら側に置いてもよいが、relay 側に置くのが最も単純）：

```yaml
exporters:
  otlphttp/langfuse:
    endpoint: https://cloud.langfuse.com/api/public/otel   # US: us.cloud.langfuse.com
    headers: { Authorization: "Basic <base64(pk-lf-...:sk-lf-...)>" }
```

そのうえで DuDuClaw はローカルの collector を向く（`otlp_endpoint = "http://127.0.0.1:4317"`）。

> **gRPC バックエンド vs HTTP バックエンド。** `otlp_headers` は relay
> collector が必要だった「認証」という理由を取り除いた。それでも relay が
> 必要なのは、バックエンドが OTLP/gRPC をまったく受け付けない場合だけ
> （Grafana Cloud のマネージド gateway、Langfuse Cloud）。バックエンドが
> gRPC の受信エンドポイントを公開しているなら、直結すればよい。

## 補足

- span はバックグラウンドスレッドでバッチ化されてエクスポートされる。エクスポートのレイテンシが reply path に乗ることはない。
- プロセス終了時、exporter guard はバッファ中の span を best-effort で flush する。
- `sample_ratio` は SDK 側で head sampling を適用する（`TraceIdRatioBased`）。トラフィックの多いマルチ agent 環境で有用。
