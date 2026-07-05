# Observability — OpenTelemetry GenAI Tracing

DuDuClaw emits [OpenTelemetry GenAI semantic-convention](https://opentelemetry.io/docs/specs/semconv/gen-ai/) spans for every agent turn, model call, and MCP tool dispatch, and can export them over OTLP to any compatible backend (Grafana Tempo, Jaeger, Langfuse, Honeycomb, Datadog, ...).

Two independent switches:

| Switch | What it controls |
|---|---|
| Build feature `otel` | Compiles the OTLP export pipeline. **Default OFF** — the release binary carries zero OpenTelemetry dependencies unless built with `--features otel`. |
| `config.toml [telemetry] otlp_endpoint` (or `OTEL_EXPORTER_OTLP_ENDPOINT` env) | Activates export at runtime. Absent ⇒ the pipeline stays dormant even in an `otel` build. |

The spans themselves are plain `tracing` spans and always exist (they also feed the dashboard log stream); the feature only controls export.

## Build

```bash
cargo build --release -p duduclaw-cli --features otel
# gateway only:
cargo check -p duduclaw-gateway --features otel
```

## Configuration

`~/.duduclaw/config.toml`:

```toml
[telemetry]
# OTLP gRPC collector endpoint (the only key required to enable export).
otlp_endpoint = "http://127.0.0.1:4317"
# Optional resource service.name (default "duduclaw").
service_name = "duduclaw"
# Optional head-sampling ratio in [0.0, 1.0] (default 1.0 = keep everything).
sample_ratio = 1.0
```

- **Transport is OTLP/gRPC** (tonic, TLS via webpki roots for `https` collectors). `otlp_protocol = "http/protobuf"` is parsed for forward-compat but currently falls back to gRPC with a warning.
- Env override: `OTEL_EXPORTER_OTLP_ENDPOINT` beats (and can substitute for) `otlp_endpoint`, so an `otel` build can be pointed at a collector without editing config.
- Custom auth headers are not (yet) supported on the exporter — for backends that require them (e.g. Langfuse, Grafana Cloud), run a local [OTel Collector](https://opentelemetry.io/docs/collector/) and let it add the credentials (examples below).
- Fail-safe: exporter init failure logs a warning to stderr and disables export — it never blocks gateway startup.

> **Log level matters.** The GenAI spans are INFO-level and obey the global
> log filter (default `warn`). When enabling telemetry, set
> `[general] log_level = "info"` in `config.toml` (or run with
> `RUST_LOG=info`), otherwise no spans are exported.

## Emitted spans

Attribute keys are centralized in `crates/duduclaw-gateway/src/otel.rs` (`attrs` module — the GenAI semconv is still "Development" status, so a spec rename is a one-file change). Both the legacy `gen_ai.system` and its successor `gen_ai.provider.name` are emitted.

| Span | Where | Attributes |
|---|---|---|
| `invoke_agent` | Channel-reply entry (`channel_reply::build_reply_with_session_inner`) and dispatcher agent run (`claude_runner::call_claude_for_agent_with_type`) | `gen_ai.operation.name=invoke_agent`, `gen_ai.system` / `gen_ai.provider.name`, `gen_ai.agent.name`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens` (usage recorded post-hoc from the CLI `result` event) |
| `chat` | Multi-runtime choke-point (`runtime_dispatch::run_agent_prompt`) and Direct API calls (`claude_runner::try_direct_api` / `try_llm_provider_api`) | `gen_ai.operation.name=chat`, `gen_ai.system` / `gen_ai.provider.name` (provider that actually answered, post-failover), `gen_ai.agent.name`, `gen_ai.request.model`, `gen_ai.usage.input_tokens`, `gen_ai.usage.output_tokens` |
| `execute_tool` | MCP tool dispatch (`duduclaw-cli::mcp_dispatch::dispatch_tool_call`; all transports — stdio / HTTP / SSE) | `gen_ai.operation.name=execute_tool`, `gen_ai.tool.name`, `gen_ai.tool.outcome` (`ok`/`error`), `error.type` on failure (scope / rate-limit / whitelist rejection or JSON-RPC error result) |

`chat` spans nest under the active `invoke_agent` span, so backends show one trace per agent turn with its model calls as children.

## Backend examples

### Local quick-start (Jaeger all-in-one)

```bash
docker run --rm -p 16686:16686 -p 4317:4317 jaegertracing/all-in-one:latest
# config.toml → otlp_endpoint = "http://127.0.0.1:4317", then open http://localhost:16686
```

### Grafana (Tempo / Alloy)

Point at the collector's OTLP gRPC port:

```toml
[telemetry]
otlp_endpoint = "http://127.0.0.1:4317"
```

For Grafana Cloud (needs Basic auth), relay through a local OTel Collector:

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

Langfuse ingests OTLP over HTTP with Basic auth — same relay pattern:

```yaml
exporters:
  otlphttp/langfuse:
    endpoint: https://cloud.langfuse.com/api/public/otel   # US: us.cloud.langfuse.com
    headers: { Authorization: "Basic <base64(pk-lf-...:sk-lf-...)>" }
```

DuDuClaw then targets the local collector (`otlp_endpoint = "http://127.0.0.1:4317"`).

## Notes

- Spans are batched and exported from a background thread; export latency never sits on the reply path.
- On process exit the exporter guard flushes buffered spans (best-effort).
- `sample_ratio` applies head sampling at the SDK (`TraceIdRatioBased`) — useful for high-traffic multi-agent fleets.
