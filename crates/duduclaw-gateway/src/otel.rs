//! OpenTelemetry GenAI tracing with OTLP export — **DEFAULT OFF**.
//!
//! This module gives DuDuClaw spans that follow the OpenTelemetry **GenAI
//! semantic conventions** (`gen_ai.*`) so agent invocations and tool calls can
//! be exported over OTLP to Langfuse / Grafana Tempo / Datadog / any OTLP
//! collector. It is engineered to cost **nothing** unless an operator opts in:
//!
//!   * The heavy OpenTelemetry SDK + OTLP exporter live behind the optional
//!     `otel` cargo feature. When the crate is built without `--features otel`
//!     (the default), [`init`] and [`subscriber_layer`] compile to no-op stubs
//!     — callers need no `cfg` guards.
//!   * Even with the feature on, tracing is only installed when
//!     `[telemetry] otlp_endpoint` is present in `<home>/config.toml`. An unset
//!     endpoint means the OTLP layer is never built → zero export overhead.
//!   * The span-builder helpers ([`invoke_agent_span`], [`execute_tool_span`],
//!     …) are **pure `tracing`** and always compiled. With no OTLP layer
//!     installed and the default `warn` log filter, these `info`-level spans are
//!     *disabled* and construct in a few nanoseconds — a genuine no-op path.
//!
//! ## GenAI semantic conventions are still "Development"
//!
//! The `gen_ai.*` attribute set is **not yet Stable** in the OpenTelemetry
//! spec — it ships under the "Development" stability level and evolves. The
//! upstream escape hatch is the `OTEL_SEMCONV_STABILITY_OPT_IN` environment
//! variable, which lets an application pin old vs. new attribute spellings
//! across a spec migration (e.g. `gen_ai.system` → `gen_ai.provider.name`). To
//! make future spec churn a **one-file change**, every attribute key is
//! centralised in [`attrs`]; the span-macro field names below must mirror those
//! constants (a unit test pins the constant values).
//! See <https://opentelemetry.io/docs/specs/semconv/gen-ai/>.
//!
//! ## Wire-up (all sites instrumented)
//!
//!   1. **Subscriber registration** — `duduclaw-cli/src/lib.rs::entry_point()`
//!      (the sole `tracing_subscriber::registry()…​.init()` site):
//!      ```ignore
//!      let _otel_guard = duduclaw_gateway::otel::init(&duduclaw_home());
//!      // …existing .with(env_filter).with(fmt)… chain…
//!      .with(duduclaw_gateway::otel::subscriber_layer())
//!      .init();
//!      // guard held to end of entry_point ⇒ flush on process exit
//!      ```
//!      [`init`] installs the SDK tracer provider (idempotent — the
//!      `start_gateway` call is a no-op when `entry_point` already installed
//!      it); [`subscriber_layer`] bridges `tracing` spans → OTLP using that
//!      provider's tracer, so **call [`init`] first** — before a provider is
//!      installed it returns `None` (a pass-through layer).
//!   2. **Root `invoke_agent` spans** — `channel_reply.rs`
//!      (`build_reply_with_session_inner`, channel turns) and
//!      `claude_runner.rs` (`call_claude_for_agent_with_type`, dispatcher
//!      runs) via `#[tracing::instrument]`; usage recorded post-hoc from the
//!      CLI `result` event (`spawn_claude_cli_with_env` /
//!      `call_claude_streaming`).
//!   3. **`chat` spans** — `runtime_dispatch.rs::run_agent_prompt`
//!      (multi-runtime choke-point) and `claude_runner.rs::try_direct_api` /
//!      `try_llm_provider_api` (Direct API).
//!   4. **`execute_tool` span** —
//!      `duduclaw-cli/src/mcp_dispatch.rs::McpDispatcher::dispatch_tool_call`
//!      (all MCP transports).

use std::path::Path;

/// Centralised GenAI semantic-convention attribute keys.
///
/// Keep this the **single source of truth**. The `tracing` span macros below
/// use dotted field-name literals that must match these constants byte-for-byte
/// (a test in this module pins the values). When the OTel GenAI spec bumps a
/// key (it is still "Development"; see `OTEL_SEMCONV_STABILITY_OPT_IN` in the
/// module docs), edit it here and update the mirrored macro literal in one place.
pub mod attrs {
    /// `gen_ai.operation.name` — the GenAI operation ("invoke_agent" / "execute_tool").
    pub const OPERATION_NAME: &str = "gen_ai.operation.name";
    /// `gen_ai.agent.name` — human/agent identifier for an `invoke_agent` span.
    pub const AGENT_NAME: &str = "gen_ai.agent.name";
    /// `gen_ai.request.model` — model id requested for the operation.
    pub const REQUEST_MODEL: &str = "gen_ai.request.model";
    /// `gen_ai.system` — legacy provider key (kept for back-compat collectors).
    pub const SYSTEM: &str = "gen_ai.system";
    /// `gen_ai.provider.name` — current provider key (replaces `gen_ai.system`).
    pub const PROVIDER_NAME: &str = "gen_ai.provider.name";
    /// `gen_ai.tool.name` — tool identifier for an `execute_tool` span.
    pub const TOOL_NAME: &str = "gen_ai.tool.name";

    /// `gen_ai.usage.input_tokens` — prompt/input token count.
    pub const USAGE_INPUT_TOKENS: &str = "gen_ai.usage.input_tokens";
    /// `gen_ai.usage.output_tokens` — completion/output token count.
    pub const USAGE_OUTPUT_TOKENS: &str = "gen_ai.usage.output_tokens";
    /// `gen_ai.usage.cache_read_input_tokens` — cached-prompt tokens (extra;
    /// Anthropic/OpenAI prompt-cache attribution, not yet in stable semconv).
    pub const USAGE_CACHE_READ_INPUT_TOKENS: &str = "gen_ai.usage.cache_read_input_tokens";
    /// `gen_ai.usage.reasoning_tokens` — extended-thinking tokens (extra).
    pub const USAGE_REASONING_TOKENS: &str = "gen_ai.usage.reasoning_tokens";

    /// `gen_ai.tool.outcome` — "ok" / "error" summary of a tool call (extra).
    pub const TOOL_OUTCOME: &str = "gen_ai.tool.outcome";
    /// `error.type` — standard OTel error attribute set on failed tool calls.
    pub const ERROR_TYPE: &str = "error.type";

    // ── `gen_ai.operation.name` values ───────────────────────────────────
    /// Value of [`OPERATION_NAME`] for an agent invocation.
    pub const OP_INVOKE_AGENT: &str = "invoke_agent";
    /// Value of [`OPERATION_NAME`] for a tool execution.
    pub const OP_EXECUTE_TOOL: &str = "execute_tool";
}

// ── Configuration ────────────────────────────────────────────────────────────

/// OTLP wire protocol. Only [`OtlpProtocol::Grpc`] is compiled (the `grpc-tonic`
/// exporter); `http/protobuf` is parsed for forward-compat but currently falls
/// back to gRPC with a warning (avoids pulling a second `reqwest` major).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtlpProtocol {
    Grpc,
    HttpProtobuf,
}

impl OtlpProtocol {
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "http" | "http/protobuf" | "http-protobuf" | "httpprotobuf" => Self::HttpProtobuf,
            _ => Self::Grpc,
        }
    }
}

/// Parsed `[telemetry]` config. Presence of a `TelemetryConfig` **is** the
/// install decision: [`TelemetryConfig::from_home`] returns `Some` only when a
/// usable `otlp_endpoint` is configured, `None` otherwise (no file, malformed
/// TOML, missing section, or blank endpoint → no telemetry, zero cost).
#[derive(Debug, Clone, PartialEq)]
pub struct TelemetryConfig {
    /// OTLP collector endpoint, e.g. `http://127.0.0.1:4317`.
    pub otlp_endpoint: String,
    /// Wire protocol (default gRPC).
    pub otlp_protocol: OtlpProtocol,
    /// Resource `service.name` (default `"duduclaw"`).
    pub service_name: String,
    /// Head sampling ratio in `[0.0, 1.0]` (default `1.0` = sample everything).
    pub sample_ratio: f64,
}

const DEFAULT_SERVICE_NAME: &str = "duduclaw";
const DEFAULT_SAMPLE_RATIO: f64 = 1.0;

impl TelemetryConfig {
    /// Read + parse `<home>/config.toml`. Returns `None` (⇒ do not install) on
    /// any failure — this is the fail-safe boundary: a missing/broken config
    /// must never prevent telemetry-less boot, and must never panic.
    pub fn from_home(home_dir: &Path) -> Option<Self> {
        let raw = std::fs::read_to_string(home_dir.join("config.toml")).ok()?;
        Self::parse(&raw)
    }

    /// Pure parser over a TOML string — the unit-tested install-decision core.
    ///
    /// Install requires a non-blank `[telemetry] otlp_endpoint`. Everything
    /// else has a safe default. `sample_ratio` is clamped to `[0.0, 1.0]`.
    pub fn parse(raw: &str) -> Option<Self> {
        let value: toml::Value = raw.parse().ok()?;
        let table = value.get("telemetry")?;

        let endpoint = table
            .get("otlp_endpoint")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())?; // blank / missing endpoint ⇒ no install

        let protocol = table
            .get("otlp_protocol")
            .and_then(|v| v.as_str())
            .map(OtlpProtocol::parse)
            .unwrap_or(OtlpProtocol::Grpc);

        let service_name = table
            .get("service_name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_SERVICE_NAME)
            .to_string();

        let sample_ratio = table
            .get("sample_ratio")
            .and_then(|v| v.as_float())
            .unwrap_or(DEFAULT_SAMPLE_RATIO)
            .clamp(0.0, 1.0);

        Some(Self {
            otlp_endpoint: endpoint.to_string(),
            otlp_protocol: protocol,
            service_name,
            sample_ratio,
        })
    }
}

// ── GenAI span helpers (pure `tracing`, always compiled) ─────────────────────
//
// These build `info`-level spans whose fields map 1:1 to GenAI attributes when
// the OTLP layer is installed, and are near-free no-ops otherwise. The dotted
// field-name literals MUST mirror `attrs::*` (pinned by `attr_keys_are_stable`).

/// Root span for an agent invocation (GenAI `invoke_agent`).
///
/// The usage fields are declared as `Empty` up front so [`record_usage`] can
/// fill them after the model call returns.
pub fn invoke_agent_span(agent_name: &str, model: &str, provider: &str) -> tracing::Span {
    tracing::info_span!(
        "invoke_agent",
        gen_ai.operation.name = attrs::OP_INVOKE_AGENT,
        gen_ai.agent.name = agent_name,
        gen_ai.request.model = model,
        gen_ai.system = provider,
        gen_ai.provider.name = provider,
        gen_ai.usage.input_tokens = tracing::field::Empty,
        gen_ai.usage.output_tokens = tracing::field::Empty,
        gen_ai.usage.cache_read_input_tokens = tracing::field::Empty,
        gen_ai.usage.reasoning_tokens = tracing::field::Empty,
    )
}

/// Record token usage onto an [`invoke_agent_span`]. `input`/`output` are the
/// standard GenAI usage attributes; `cache_read`/`reasoning` are extra
/// namespaced attributes. Recording a field on a disabled span is a no-op.
pub fn record_usage(span: &tracing::Span, input: u64, output: u64, cache_read: u64, reasoning: u64) {
    span.record(attrs::USAGE_INPUT_TOKENS, input);
    span.record(attrs::USAGE_OUTPUT_TOKENS, output);
    span.record(attrs::USAGE_CACHE_READ_INPUT_TOKENS, cache_read);
    span.record(attrs::USAGE_REASONING_TOKENS, reasoning);
}

/// Span for a single MCP tool execution (GenAI `execute_tool`).
///
/// Outcome fields are declared `Empty`; call [`record_tool_outcome`] after the
/// handler returns.
pub fn execute_tool_span(tool_name: &str) -> tracing::Span {
    tracing::info_span!(
        "execute_tool",
        gen_ai.operation.name = attrs::OP_EXECUTE_TOOL,
        gen_ai.tool.name = tool_name,
        gen_ai.tool.outcome = tracing::field::Empty,
        error.type = tracing::field::Empty,
    )
}

/// Record the ok/error outcome of a tool call onto an [`execute_tool_span`].
pub fn record_tool_outcome(span: &tracing::Span, ok: bool) {
    span.record(attrs::TOOL_OUTCOME, if ok { "ok" } else { "error" });
    if !ok {
        span.record(attrs::ERROR_TYPE, "tool_error");
    }
}

// ── OTLP provider install (feature-gated) ────────────────────────────────────

#[cfg(feature = "otel")]
mod exporter {
    use super::{OtlpProtocol, TelemetryConfig};
    use std::sync::OnceLock;

    /// The installed provider. `tracing-opentelemetry`'s bridge layer needs a
    /// concrete SDK tracer at construction time (the boxed `global::tracer()`
    /// does not implement `PreSampledTracer`), so [`super::subscriber_layer`]
    /// reads it from here. Also makes [`install`] idempotent: `entry_point`
    /// installs first, the later `start_gateway` call is a no-op.
    static PROVIDER: OnceLock<opentelemetry_sdk::trace::SdkTracerProvider> = OnceLock::new();

    /// The installed provider, if [`install`] has run successfully.
    pub(super) fn provider() -> Option<&'static opentelemetry_sdk::trace::SdkTracerProvider> {
        PROVIDER.get()
    }

    /// RAII guard: flushes and shuts the OTLP exporter down on drop so buffered
    /// spans are not lost at process exit. Best-effort — drop never panics.
    pub struct OtelGuard {
        provider: opentelemetry_sdk::trace::SdkTracerProvider,
    }

    impl Drop for OtelGuard {
        fn drop(&mut self) {
            // Flush pending spans, then release exporter resources. Errors on a
            // dying process are not actionable; swallow them.
            let _ = self.provider.force_flush();
            let _ = self.provider.shutdown();
        }
    }

    /// Build the SDK tracer provider + OTLP exporter and register it as the
    /// global provider. Returns `Err` on any exporter/build failure so the
    /// caller can warn-and-continue (fail-safe).
    ///
    /// Must be called from within a Tokio runtime: the gRPC (tonic) exporter
    /// builds a lazily-connecting channel that expects a reactor handle. Both
    /// documented call sites ([`super::init`] via `start_gateway`, and the
    /// `entry_point` follow-up) run inside `#[tokio::main]`.
    pub fn install(cfg: &TelemetryConfig) -> Result<OtelGuard, Box<dyn std::error::Error>> {
        use opentelemetry_otlp::WithExportConfig; // brings `.with_endpoint` into scope
        use opentelemetry_sdk::trace::{Sampler, SdkTracerProvider};
        use opentelemetry_sdk::Resource;

        // Idempotent: a second install (start_gateway after entry_point) must
        // not stack a second batch pipeline over the first.
        if PROVIDER.get().is_some() {
            return Err("OpenTelemetry provider already installed".into());
        }

        if cfg.otlp_protocol == OtlpProtocol::HttpProtobuf {
            tracing::warn!(
                "[telemetry] otlp_protocol='http/protobuf' requested, but only the \
                 grpc-tonic exporter is compiled; falling back to gRPC against the \
                 configured endpoint"
            );
        }

        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(cfg.otlp_endpoint.clone())
            .build()?;

        let resource = Resource::builder()
            .with_service_name(cfg.service_name.clone())
            .build();

        // Batch processor runs on its own background thread → no runtime coupling
        // for export itself; only the tonic channel build wants a reactor.
        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .with_resource(resource)
            .with_sampler(Sampler::TraceIdRatioBased(cfg.sample_ratio))
            .build();

        opentelemetry::global::set_tracer_provider(provider.clone());
        let _ = PROVIDER.set(provider.clone());
        Ok(OtelGuard { provider })
    }
}

#[cfg(feature = "otel")]
pub use exporter::OtelGuard;

/// Initialise OTLP tracing from `<home>/config.toml`. Returns a guard that
/// flushes spans on drop, or `None` when telemetry is not configured / the
/// `otel` feature is off. **Fail-safe**: any exporter error is logged at `warn`
/// and swallowed — telemetry never blocks the gateway from booting.
///
/// Call this from the process bootstrap (see the module doc for the exact
/// subscriber wire-up); `start_gateway` calls it for the gateway process.
#[cfg(feature = "otel")]
pub fn init(home_dir: &Path) -> Option<OtelGuard> {
    // Idempotent: entry_point installs first; the later start_gateway call
    // finds a provider and quietly no-ops (the first guard stays the owner).
    if exporter::provider().is_some() {
        return None;
    }
    // Standard OTLP env override: OTEL_EXPORTER_OTLP_ENDPOINT beats (and can
    // substitute for) `[telemetry] otlp_endpoint`, so operators can point an
    // otel-enabled build at a collector without editing config.toml.
    let env_endpoint = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let cfg = match (TelemetryConfig::from_home(home_dir), env_endpoint) {
        (Some(mut cfg), Some(endpoint)) => {
            cfg.otlp_endpoint = endpoint;
            cfg
        }
        (Some(cfg), None) => cfg,
        (None, Some(endpoint)) => TelemetryConfig {
            otlp_endpoint: endpoint,
            otlp_protocol: OtlpProtocol::Grpc,
            service_name: DEFAULT_SERVICE_NAME.to_string(),
            sample_ratio: DEFAULT_SAMPLE_RATIO,
        },
        (None, None) => return None,
    };
    match exporter::install(&cfg) {
        Ok(guard) => {
            // `init` runs before the tracing subscriber is installed
            // (entry_point), so a tracing::info! here would be lost — mirror
            // the "[duduclaw] effective log level" stderr pattern instead.
            eprintln!(
                "[duduclaw] OpenTelemetry OTLP GenAI tracing enabled → {} (service {}, sample {})",
                cfg.otlp_endpoint, cfg.service_name, cfg.sample_ratio
            );
            Some(guard)
        }
        Err(e) => {
            eprintln!(
                "[duduclaw] OpenTelemetry init failed ({e}); continuing without tracing export"
            );
            None
        }
    }
}

/// `tracing` → OTLP bridge layer for the subscriber registry. Add it with
/// `.with(otel::subscriber_layer())` **after** calling [`init`]: the bridge
/// needs the installed SDK provider's concrete tracer (`global::tracer()`'s
/// boxed tracer doesn't implement `PreSampledTracer`, so lazy resolution is
/// not possible). Returns `None` — a pass-through layer — when [`init`] has
/// not installed a provider (telemetry unconfigured or init failed).
#[cfg(feature = "otel")]
pub fn subscriber_layer<S>() -> Option<impl tracing_subscriber::Layer<S>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    use opentelemetry::trace::TracerProvider as _;
    let provider = exporter::provider()?;
    Some(tracing_opentelemetry::layer().with_tracer(provider.tracer("duduclaw")))
}

// ── No-op stubs (feature `otel` OFF) ─────────────────────────────────────────
// Same signatures as the feature-on versions so callers never need cfg guards.

/// No-op guard when the `otel` feature is disabled.
#[cfg(not(feature = "otel"))]
#[derive(Debug)]
pub struct OtelGuard;

/// No-op: telemetry is compiled out. Always returns `None` (zero cost).
#[cfg(not(feature = "otel"))]
pub fn init(_home_dir: &Path) -> Option<OtelGuard> {
    None
}

/// No-op subscriber layer when the `otel` feature is disabled. Returns `None`
/// (`Option<Identity>` is a pass-through layer), so
/// `.with(otel::subscriber_layer())` composes cleanly regardless of feature
/// state. No `<S>` parameter here: with `Identity` in the return type a type
/// parameter would be unconstrained at the call site and fail inference.
#[cfg(not(feature = "otel"))]
pub fn subscriber_layer() -> Option<tracing_subscriber::layer::Identity> {
    None
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_endpoint_present_installs_with_defaults() {
        let cfg = TelemetryConfig::parse("[telemetry]\notlp_endpoint = \"http://127.0.0.1:4317\"\n")
            .expect("endpoint present ⇒ install");
        assert_eq!(cfg.otlp_endpoint, "http://127.0.0.1:4317");
        assert_eq!(cfg.otlp_protocol, OtlpProtocol::Grpc);
        assert_eq!(cfg.service_name, "duduclaw");
        assert_eq!(cfg.sample_ratio, 1.0);
    }

    #[test]
    fn parse_endpoint_absent_no_install() {
        // Section present but no endpoint ⇒ no install.
        assert!(TelemetryConfig::parse("[telemetry]\nservice_name = \"x\"\n").is_none());
        // Blank endpoint ⇒ no install.
        assert!(TelemetryConfig::parse("[telemetry]\notlp_endpoint = \"   \"\n").is_none());
    }

    #[test]
    fn parse_no_telemetry_section_no_install() {
        assert!(TelemetryConfig::parse("[general]\nlog_level = \"info\"\n").is_none());
    }

    #[test]
    fn parse_malformed_toml_no_install() {
        assert!(TelemetryConfig::parse("this is = = not toml [[[").is_none());
        assert!(TelemetryConfig::parse("").is_none());
    }

    #[test]
    fn parse_custom_values_and_protocol() {
        let raw = "[telemetry]\n\
            otlp_endpoint = \"https://cloud.langfuse.com:4317\"\n\
            otlp_protocol = \"http/protobuf\"\n\
            service_name = \"duduclaw-prod\"\n\
            sample_ratio = 0.25\n";
        let cfg = TelemetryConfig::parse(raw).unwrap();
        assert_eq!(cfg.otlp_protocol, OtlpProtocol::HttpProtobuf);
        assert_eq!(cfg.service_name, "duduclaw-prod");
        assert_eq!(cfg.sample_ratio, 0.25);
    }

    #[test]
    fn sample_ratio_is_clamped() {
        let over =
            TelemetryConfig::parse("[telemetry]\notlp_endpoint = \"h\"\nsample_ratio = 5.0\n")
                .unwrap();
        assert_eq!(over.sample_ratio, 1.0);
        let under =
            TelemetryConfig::parse("[telemetry]\notlp_endpoint = \"h\"\nsample_ratio = -2.0\n")
                .unwrap();
        assert_eq!(under.sample_ratio, 0.0);
    }

    #[test]
    fn protocol_parse_defaults_to_grpc() {
        assert_eq!(OtlpProtocol::parse("grpc"), OtlpProtocol::Grpc);
        assert_eq!(OtlpProtocol::parse("GRPC"), OtlpProtocol::Grpc);
        assert_eq!(OtlpProtocol::parse("nonsense"), OtlpProtocol::Grpc);
        assert_eq!(OtlpProtocol::parse("http"), OtlpProtocol::HttpProtobuf);
        assert_eq!(OtlpProtocol::parse("http/protobuf"), OtlpProtocol::HttpProtobuf);
    }

    /// Pins the GenAI attribute keys. If the OTel semconv (still "Development")
    /// renames a key, update `attrs` + the mirrored macro literal, then this test.
    #[test]
    fn attr_keys_are_stable() {
        assert_eq!(attrs::OPERATION_NAME, "gen_ai.operation.name");
        assert_eq!(attrs::AGENT_NAME, "gen_ai.agent.name");
        assert_eq!(attrs::REQUEST_MODEL, "gen_ai.request.model");
        assert_eq!(attrs::SYSTEM, "gen_ai.system");
        assert_eq!(attrs::PROVIDER_NAME, "gen_ai.provider.name");
        assert_eq!(attrs::TOOL_NAME, "gen_ai.tool.name");
        assert_eq!(attrs::USAGE_INPUT_TOKENS, "gen_ai.usage.input_tokens");
        assert_eq!(attrs::USAGE_OUTPUT_TOKENS, "gen_ai.usage.output_tokens");
        assert_eq!(
            attrs::USAGE_CACHE_READ_INPUT_TOKENS,
            "gen_ai.usage.cache_read_input_tokens"
        );
        assert_eq!(attrs::USAGE_REASONING_TOKENS, "gen_ai.usage.reasoning_tokens");
        assert_eq!(attrs::TOOL_OUTCOME, "gen_ai.tool.outcome");
        assert_eq!(attrs::ERROR_TYPE, "error.type");
        assert_eq!(attrs::OP_INVOKE_AGENT, "invoke_agent");
        assert_eq!(attrs::OP_EXECUTE_TOOL, "execute_tool");
    }

    #[test]
    fn init_without_config_is_none() {
        // No config.toml in an empty dir ⇒ no install (holds for both feature
        // states: feature-off is an unconditional None; feature-on returns None
        // because `from_home` fails to read the file).
        let dir = tempfile::tempdir().unwrap();
        assert!(init(dir.path()).is_none());
    }

    #[test]
    fn span_helpers_construct_and_record_without_panic() {
        // Exercises the always-compiled no-op span path (no subscriber installed
        // in the test ⇒ disabled spans). Must not panic and must be cheap.
        let agent = invoke_agent_span("scout", "claude-sonnet-4-6", "anthropic");
        record_usage(&agent, 1200, 340, 900, 128);

        let tool = execute_tool_span("memory_store");
        record_tool_outcome(&tool, true);
        let failed = execute_tool_span("odoo.execute");
        record_tool_outcome(&failed, false);
    }
}
