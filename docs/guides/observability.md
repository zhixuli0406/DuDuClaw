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
# Optional auth headers sent with every export request (gRPC metadata) — lets
# DuDuClaw talk directly to authenticated OTLP backends, no relay collector
# needed. Keys are normalized to lowercase; invalid entries are skipped with a
# warning (fail-safe, never blocks boot).
otlp_headers = { authorization = "Basic <base64(user:token)>", "x-api-key" = "yyy" }
```

- **Transport is OTLP/gRPC** (tonic, TLS via webpki roots for `https` collectors). `otlp_protocol = "http/protobuf"` is parsed for forward-compat but currently falls back to gRPC with a warning.
- Env overrides (both standard OTLP conventions):
  - `OTEL_EXPORTER_OTLP_ENDPOINT` beats (and can substitute for) `otlp_endpoint`.
  - `OTEL_EXPORTER_OTLP_HEADERS` — comma-separated `key=value` pairs, merged **over** `otlp_headers` (env wins per-key). Values may be percent-encoded, e.g. `Authorization=Basic%20<base64>`. Handy for keeping credentials out of `config.toml`.
- Header validity: keys must be lowercase-able ASCII from `[a-z0-9_.-]` without the reserved gRPC `-bin` suffix; values must be visible ASCII. Anything else is skipped with a stderr warning — a bad header never panics or aborts export of the rest.
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

### Authenticated OTLP/gRPC backends — direct, no collector

With `otlp_headers`, DuDuClaw exports **directly** to any backend that accepts
OTLP/gRPC with auth headers (Honeycomb, Dash0, self-hosted Tempo/Mimir behind
an auth proxy, ...) — the local OTel Collector relay is optional for these:

```toml
[telemetry]
otlp_endpoint = "https://api.honeycomb.io:443"
otlp_headers = { "x-honeycomb-team" = "<api-key>" }
```

Or Basic auth (self-hosted Grafana Tempo behind basic-auth, gRPC-capable
gateways):

```toml
[telemetry]
otlp_endpoint = "https://tempo.example.com:4317"
otlp_headers = { authorization = "Basic <base64(user:token)>" }
```

Prefer env for the credential (merges over config, env wins per-key):

```bash
export OTEL_EXPORTER_OTLP_HEADERS="Authorization=Basic%20<base64(user:token)>"
```

### Grafana (local Tempo / Alloy)

Point at the collector's OTLP gRPC port:

```toml
[telemetry]
otlp_endpoint = "http://127.0.0.1:4317"
```

A local Alloy/Collector can also relay to Grafana Cloud — with `otlp_headers`
it now injects the credential itself, but the relay is still what converts
transport, because Grafana Cloud's managed OTLP gateway ingests **OTLP/HTTP
only** (DuDuClaw's exporter speaks OTLP/gRPC):

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

Langfuse Cloud's OTLP endpoint (`/api/public/otel`) likewise ingests
**OTLP/HTTP only**, so keep the minimal gRPC→HTTP relay (auth can live on
either side; simplest to keep it in the relay):

```yaml
exporters:
  otlphttp/langfuse:
    endpoint: https://cloud.langfuse.com/api/public/otel   # US: us.cloud.langfuse.com
    headers: { Authorization: "Basic <base64(pk-lf-...:sk-lf-...)>" }
```

DuDuClaw then targets the local collector (`otlp_endpoint = "http://127.0.0.1:4317"`).

> **gRPC vs HTTP backends.** `otlp_headers` removes the *auth* reason for a
> relay collector; a relay is only still needed where the backend refuses
> OTLP/gRPC entirely (Grafana Cloud's managed gateway, Langfuse Cloud). If a
> backend exposes a gRPC ingest endpoint, go direct.

## Notes

- Spans are batched and exported from a background thread; export latency never sits on the reply path.
- On process exit the exporter guard flushes buffered spans (best-effort).
- `sample_ratio` applies head sampling at the SDK (`TraceIdRatioBased`) — useful for high-traffic multi-agent fleets.
