# 可觀測性：OpenTelemetry GenAI 追蹤

DuDuClaw 會替每一次 agent 回合、模型呼叫與 MCP 工具派發，發出符合 [OpenTelemetry GenAI 語意慣例](https://opentelemetry.io/docs/specs/semconv/gen-ai/) 的 span，並可透過 OTLP 匯出到任何相容的後端（Grafana Tempo、Jaeger、Langfuse、Honeycomb、Datadog……）。

兩個各自獨立的開關：

| 開關 | 作用 |
|---|---|
| Build feature `otel` | 編譯 OTLP 匯出流水線。**預設關閉**：除非用 `--features otel` 建置，否則 release binary 完全不帶任何 OpenTelemetry 依賴。 |
| `config.toml [telemetry] otlp_endpoint`（或 `OTEL_EXPORTER_OTLP_ENDPOINT` 環境變數） | 在執行期啟用匯出。未設定時，即使是 `otel` build，流水線依然保持休眠。 |

這些 span 本身只是普通的 `tracing` span，一直都存在（同時也會餵給 dashboard 的 log stream）；這個 feature 只控制要不要匯出。

## 建置

```bash
cargo build --release -p duduclaw-cli --features otel
# 只建置 gateway：
cargo check -p duduclaw-gateway --features otel
```

## 設定

`~/.duduclaw/config.toml`：

```toml
[telemetry]
# OTLP gRPC collector 的端點（唯一需要設定才能啟用匯出的鍵）。
otlp_endpoint = "http://127.0.0.1:4317"
# 選填的 resource service.name（預設為 "duduclaw"）。
service_name = "duduclaw"
# 選填的 head-sampling 比例，範圍 [0.0, 1.0]（預設 1.0，代表全部保留）。
sample_ratio = 1.0
# 選填的驗證標頭，會隨每一次匯出請求送出（gRPC metadata），讓 DuDuClaw
# 可以直接和需要驗證的 OTLP 後端對話，不必架 relay collector。鍵名會
# 正規化成小寫；無效項目會被跳過並輸出警告（fail-safe，絕不擋住開機）。
otlp_headers = { authorization = "Basic <base64(user:token)>", "x-api-key" = "yyy" }
```

- **傳輸層是 OTLP/gRPC**（tonic，`https` collector 走 webpki roots 做 TLS）。`otlp_protocol = "http/protobuf"` 會被解析以求向前相容，但目前一律退回 gRPC 並輸出警告。
- 環境變數覆寫（都是標準 OTLP 慣例）：
  - `OTEL_EXPORTER_OTLP_ENDPOINT` 優先於（也可以直接取代）`otlp_endpoint`。
  - `OTEL_EXPORTER_OTLP_HEADERS`：以逗號分隔的 `key=value` 組合，會**疊加在** `otlp_headers` 之上（同一個鍵以環境變數為準）。值可以是 percent-encoded，例如 `Authorization=Basic%20<base64>`。很適合用來把憑證留在 `config.toml` 之外。
- 標頭合法性：鍵名必須是可轉小寫的 ASCII，字元集為 `[a-z0-9_.-]`，且不能帶 gRPC 保留的 `-bin` 後綴；值必須是可見 ASCII。不符合的一律跳過並輸出 stderr 警告，不合法的標頭不會 panic，也不會讓其餘匯出中斷。
- Fail-safe：exporter 初始化失敗時只會輸出 stderr 警告並停用匯出，絕不會擋住 gateway 開機。

> **日誌層級很重要。** GenAI span 是 INFO 等級，會受全域日誌過濾規則（預設
> `warn`）限制。啟用 telemetry 時，記得在 `config.toml` 設定
> `[general] log_level = "info"`（或用 `RUST_LOG=info` 執行），否則不會有
> 任何 span 被匯出。

## 發送的 Span

屬性鍵集中定義在 `crates/duduclaw-gateway/src/otel.rs`（`attrs` module；GenAI semconv 目前仍是「Development」狀態，規格改名時只需要改這一個檔案）。舊版的 `gen_ai.system` 與取代它的 `gen_ai.provider.name` 兩者都會發出。

| Span | 位置 | Attributes |
|---|---|---|
| `invoke_agent` | Channel-reply 進入點（`channel_reply::build_reply_with_session_inner`）與 dispatcher 的 agent 執行（`claude_runner::call_claude_for_agent_with_type`） | `gen_ai.operation.name=invoke_agent`、`gen_ai.system` / `gen_ai.provider.name`、`gen_ai.agent.name`、`gen_ai.request.model`、`gen_ai.usage.input_tokens`、`gen_ai.usage.output_tokens`（usage 是事後從 CLI 的 `result` 事件記錄回來的） |
| `chat` | Multi-runtime 的關卡（`runtime_dispatch::run_agent_prompt`）與 Direct API 呼叫（`claude_runner::try_direct_api` / `try_llm_provider_api`） | `gen_ai.operation.name=chat`、`gen_ai.system` / `gen_ai.provider.name`（實際回答的 provider，failover 之後的結果）、`gen_ai.agent.name`、`gen_ai.request.model`、`gen_ai.usage.input_tokens`、`gen_ai.usage.output_tokens` |
| `execute_tool` | MCP 工具派發（`duduclaw-cli::mcp_dispatch::dispatch_tool_call`；涵蓋所有 transport：stdio／HTTP／SSE） | `gen_ai.operation.name=execute_tool`、`gen_ai.tool.name`、`gen_ai.tool.outcome`（`ok`/`error`）、失敗時的 `error.type`（scope／rate-limit／whitelist 拒絕，或 JSON-RPC 錯誤結果） |

`chat` span 會巢狀掛在當前的 `invoke_agent` span 底下，因此後端看到的是每個 agent 回合一條 trace，模型呼叫則是它的子節點。

## 後端範例

### 本機快速上手（Jaeger all-in-one）

```bash
docker run --rm -p 16686:16686 -p 4317:4317 jaegertracing/all-in-one:latest
# config.toml → otlp_endpoint = "http://127.0.0.1:4317"，接著打開 http://localhost:16686
```

### 需要驗證的 OTLP/gRPC 後端：直連，不必架 collector

有了 `otlp_headers`，DuDuClaw 可以**直接**匯出到任何接受帶驗證標頭 OTLP/gRPC 的後端（Honeycomb、Dash0、架在驗證 proxy 後面的自架 Tempo/Mimir……），本機的 OTel Collector relay 對這些後端是選用的：

```toml
[telemetry]
otlp_endpoint = "https://api.honeycomb.io:443"
otlp_headers = { "x-honeycomb-team" = "<api-key>" }
```

或用 Basic auth（架在 basic-auth 後面的自架 Grafana Tempo、支援 gRPC 的 gateway）：

```toml
[telemetry]
otlp_endpoint = "https://tempo.example.com:4317"
otlp_headers = { authorization = "Basic <base64(user:token)>" }
```

若想把憑證放在環境變數（會疊加在 config 之上，同一個鍵以環境變數為準）：

```bash
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic%20<base64(user:token)>"
```

### Grafana（本機 Tempo／Alloy）

指向 collector 的 OTLP gRPC 埠：

```toml
[telemetry]
otlp_endpoint = "http://127.0.0.1:4317"
```

本機的 Alloy／Collector 也可以轉發到 Grafana Cloud。有了 `otlp_headers`，憑證現在可以由它自己注入，但轉發這一步仍然少不了，因為 Grafana Cloud 的受管 OTLP gateway 只吃 **OTLP/HTTP**（DuDuClaw 的 exporter 講的是 OTLP/gRPC）：

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

Langfuse Cloud 的 OTLP 端點（`/api/public/otel`）同樣只吃 **OTLP/HTTP**，所以還是需要留著這個最精簡的 gRPC→HTTP relay（驗證放在哪一側都可以，最簡單的做法是放在 relay 裡）：

```yaml
exporters:
  otlphttp/langfuse:
    endpoint: https://cloud.langfuse.com/api/public/otel   # US: us.cloud.langfuse.com
    headers: { Authorization: "Basic <base64(pk-lf-...:sk-lf-...)>" }
```

接著讓 DuDuClaw 指向本機的 collector（`otlp_endpoint = "http://127.0.0.1:4317"`）。

> **gRPC 後端 vs HTTP 後端。** `otlp_headers` 拿掉了「為了驗證才需要 relay
> collector」這個理由；真正還需要 relay 的情況，只剩後端完全不吃
> OTLP/gRPC 的時候（Grafana Cloud 的受管 gateway、Langfuse Cloud）。只要
> 後端有開放 gRPC 的接收端點，就直接連過去。

## 補充說明

- Span 會在背景執行緒批次匯出；匯出的延遲不會出現在回覆路徑上。
- 行程結束時，exporter guard 會盡力把緩衝中的 span flush 出去（best-effort）。
- `sample_ratio` 是在 SDK 層套用 head sampling（`TraceIdRatioBased`），適合流量大的多 agent 機隊。
