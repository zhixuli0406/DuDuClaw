# MCP HTTP authority for one operation

Last updated: 2026-10-25. Status: unreleased developer API in `duduclaw-llm` with the `mcp-client` feature.

A host can authorize MCP discovery or a tool call with a fresh bearer and an owned lease that lasts only for that operation. This reference describes the shared transport API. The original bearer/lease API below remains the Generic lane. The unreleased Google Personal integration uses the separate Closed lane described below; it has no new dashboard switch.

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

## Closed Google host integration (unreleased source draft)

`RegistryClient::scoped_http_closed(recipe, host_binding, guard)` opts in explicitly. The actual registry slot supplies a private-constructor `ClosedClientTarget` with the server, recipe, filter, host-entry binding and absolute deadline. `prepare_closed_client_call` must return one consuming `OwnedScopedOperation`; the default refuses. A wrong reply variant or changed final arguments is refused without falling back to the Generic bearer/lease API.

Gateway captures the actual requester before metadata offloading and pins the original parsed entry, ordinal, frame, operation nonce and original account choice. Credential owners perform the three POSTs, JSON/SSE decoding, text-block joining, known JSON reparsing, curation and final bearer/session scrub. Only their completed typed reply leaves the owner. Closed requests and each POST response are limited to 16 MiB; response limits apply per POST. Ambient proxies, redirects and retries are disabled, including the Closed credential refresh transport.

The original deadline continues through registry and loop-level result hooks, source verification, CCR preparation and admission to the next model request. Expired output, account notices and new CCR handles are withheld together. The host-only optional `ToolOutcome` field is crate-private: downstream Rust struct literals need to move to its constructors when `mcp-client` is enabled. Existing constructors still default to no Closed policy, and Generic/Persistent signatures remain unchanged.

Synchronous keychain/local operations cannot be forcibly preempted; the deadline is checked on return and before further sends. Dropping a future does not undo a refresh, remote effect or publication already started. Exact-call CCR cancellation cleanup uses the existing metadata tombstone transaction; an already-started bounded SQLite worker can finish late. A process crash or cleanup failure can leave an unadvertised row until existing expiry. No durable timeout reconciler or zero-retention guarantee is added.

All new M1 tests are source only, not compiled or run. Feature-off builds, independent code/security review, full CI and real-provider acceptance remain pending. Service-account delegation, Bridge and other W3 consumers are separate work.


Closed admission stages provenance and result metadata, then rechecks every original deadline after synchronous ledger and route construction. Expired members are discarded and the ledger is rebuilt without their spans before commit. Test builds require a separate owned Home refresh endpoint; a missing fixture refuses before crypto or provider sends. Legacy endpoint policy is unchanged.

## Source reference

- [MCP client and registry](../../crates/duduclaw-llm/src/mcp_client.rs)
- [Host interceptor and authority types](../../crates/duduclaw-llm/src/tool_loop/interceptor.rs)
- [Scoped HTTP fixtures](../../crates/duduclaw-llm/src/mcp_client/scoped_http_tests.rs)

[繁體中文](zh-TW/mcp-call-lifetimes.md) · [日本語](ja-JP/mcp-call-lifetimes.md)

### W3 account variants

The unpublished W3 source connects Home SA and Delegate through the same Closed operation contract. It also extends the existing Google account approval wire with explicit service-account/Apps Script tags and an optional tagged `home_source` projection. Old Personal/OAuth JSON still omits that field. Rust callers constructing `GoogleAccountApproval` with a struct literal must add `home_source`; this is a Rust source-compatibility change, not a byte-identical public struct API. No new dependency, token schema or approval gate is added. W3 tests remain NOTCOMPILED/NOTRUN.
