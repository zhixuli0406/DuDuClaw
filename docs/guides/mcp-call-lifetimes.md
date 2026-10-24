# MCP HTTP authority for one operation

Last updated: 2026-10-24. Status: unreleased developer API in `duduclaw-llm` with the `mcp-client` feature.

A host can authorize MCP discovery or a tool call with a fresh bearer and an owned lease that lasts only for that operation. This reference describes the shared transport API. Gateway and Google Personal integration are not connected by this change; there is no dashboard or user configuration switch for it.

## Host API

| API | Contract |
|---|---|
| `ScopedHttpRecipe::new(url, headers, operation_timeout)` | Immutable connection metadata. HTTPS is accepted; HTTP is limited to parsed localhost or loopback addresses. Rejects URL credentials/fragments, `Authorization`/`Proxy-Authorization`/`Cookie`/`Cookie2`/`Mcp-Session-Id` headers, invalid headers, zero timeout and timeouts over 300 seconds. Holds no bearer or MCP session. The host must construct it from trusted configuration. |
| `RegistryClient::scoped_http(recipe, guard)` | Couples the recipe with a host-owned `Arc<dyn ToolInterceptor>`. |
| `ToolRegistry::from_client_slots_named(clients, filters)` | Builds mixed `Persistent` and scoped slots. Scoped names must be nonempty. Binds each scoped guard to the actual client index before discovery; logical names or remote metadata cannot replace that binding. |
| `ToolInterceptor::prepare_client_call(server, operation, args)` | Prepares `ClientOperation::Discover` or `ClientOperation::Tool { name }`. Returns `PreparedClientCall` containing final arguments, `Some(lease)` and `Some(fresh_http_bearer)`; either authority field missing refuses the operation. |
| `ToolCallLease::validate_before_send(&mut self)` | Read-only, idempotent live-authority check. It must not refresh credentials or acquire another lease. |
| `SecretBearer::new(value)` | Owns the fresh bearer. Has no `Debug`, `Clone` or serialization implementation. |

An existing interceptor that only implements `before_call` or `before_call_async` cannot authorize a scoped slot: the default preparation refuses discovery and supplies neither bearer nor lease for tools. Registering another guard for an already guarded index is refused.

## Operation lifetime

Discovery obtains its own authority before `initialize`, the initialized notification and `tools/list`. A tool call waits for the exact slot's mutex, prepares authority, and creates a fresh HTTP client and MCP session. The same owned bearer and lease cover that operation's handshake and tool request; neither is cached for the next operation.

Every POST checks the lease immediately before sending, including initialization and notifications. Parsed responses are checked again before content is released; an accepted notification also rechecks authority. Validation does not recapture a different account. The host remains responsible for making the lease's authority checks correspond to the bearer returned by preparation.

The operation timeout covers waiting for the slot, preparation, handshake, HTTP response and registry result processing. Discovery has the same bounded preparation/transport lifetime. Completion, errors, timeout and cancellation drop the operation's client and owned lease. HTTP redirects and retries are disabled; there is no automatic reconnect or replay after an uncertain result. Servers that require a session to survive across separate operations are not supported by this scoped mode.

## Results and limits

Preparation failures produce a fixed refusal. Transport, response-validation and timeout failures produce a fixed unconfirmed-result message: remote execution may already have occurred. A timeout while queued does not prove anything was sent. Dropping a local future releases local authority; it does not prove a remote write was canceled. Callers must not automatically retry an unconfirmed operation.

Scoped responses redact the current bearer from returned JSON keys and values. Raw provider error bodies and arbitrary preparation errors are withheld. `SecretBearer` overwrites its own buffer on Drop on a best-effort basis; this does not guarantee erasure of copies held by the caller, allocator or HTTP library.

`RegistryClient::Persistent` keeps its existing connection and `before_call_async` path. Scoped preparation replaces that slot-level before hook rather than calling both. Existing filters, first-wins routing, `after_call`, source-attestation/CCR processing and host notices remain in the registry pipeline.

## Source reference

- [MCP client and registry](../../crates/duduclaw-llm/src/mcp_client.rs)
- [Host interceptor and authority types](../../crates/duduclaw-llm/src/tool_loop/interceptor.rs)
- [Scoped HTTP fixtures](../../crates/duduclaw-llm/src/mcp_client/scoped_http_tests.rs)

[繁體中文](zh-TW/mcp-call-lifetimes.md) · [日本語](ja-JP/mcp-call-lifetimes.md)
