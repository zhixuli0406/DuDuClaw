use super::*;

pub async fn run_mcp_server(home_dir: &Path) -> Result<()> {
    info!("Starting DuDuClaw MCP server");

    // WP21 debt ⑧ — refuse to serve an unprovable caller identity when the
    // operator has opted into strict mode. Same fail-closed shape as the M6
    // auth gate below: a server that cannot say *who* is calling cannot
    // authorize anything, and dying at boot with a legible reason beats
    // running as `__untrusted__` and answering every tool call with a
    // confusing "not in your team".
    if caller_identity_verdict(home_dir) == duduclaw_core::IdentityVerdict::Rejected {
        return Err(DuDuClawError::Gateway(format!(
            "MCP caller identity rejected: {} is missing or does not match {} \
             (checked against {}). `[delegation] require_identity_token = true` \
             in config.toml requires a DuDuClaw-issued token. Restart the gateway \
             so it re-issues every agent's MCP config, or set the flag back to \
             false to run in soft mode.",
            duduclaw_core::ENV_AGENT_TOKEN,
            duduclaw_core::ENV_AGENT_ID,
            duduclaw_core::identity_key_path(home_dir).display(),
        )));
    }

    // `os_*` tools: on an appliance every sysd call from this process goes
    // through the gateway's signed route, never the sysd socket directly
    // (sysd may be limited to the gateway's main process).
    duduclaw_gateway::sysd_route::forward_sysd_through_gateway(home_dir);

    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| DuDuClawError::Gateway(format!("Failed to create HTTP client: {e}")))?;

    // Initialize memory engine
    let memory_db_path = home_dir.join("memory.db");
    let memory = maybe_with_semantic_embedder(
        SqliteMemoryEngine::new(&memory_db_path)
            .map_err(|e| DuDuClawError::Memory(format!("Failed to open memory DB: {e}")))?,
        home_dir,
    );

    let default_agent = get_default_agent(home_dir).await;

    // ── MCP Auth 初始化（W19-P0）──────────────────────────────────
    // Gap (a), WP-H2 §1.3: `auth_cache` is created once per process and
    // shared for the lifetime of this stdio loop below — every `tools/list`
    // / `tools/call` dispatch re-authenticates through it (mtime-cached, so
    // the hot path costs one `fs::metadata` stat when nothing changed)
    // instead of trusting a `Principal` resolved once at boot. Key
    // rotation/revocation in `config.toml [mcp_keys]` is now observed on the
    // very next call, not only after a restart.
    let auth_cache = crate::mcp_auth::KeyRegistryCache::new();
    let principal = crate::mcp_auth::authenticate_from_env_cached(home_dir, &auth_cache)
        .map_err(|e| DuDuClawError::Gateway(startup_auth_error(&e)))?;
    // Memory namespace unification (v1.68.0): an internal-key caller whose
    // `DUDUCLAW_AGENT_ID` is proven by `DUDUCLAW_AGENT_TOKEN` reads and writes
    // memory under the bare employee id — the same rows the gateway distils,
    // injects and shows on the dashboard. The environment does not change for
    // the life of this process, so the proof is checked once here; without it
    // the caller stays in the old shared pool (fail closed).
    let verified_agent = crate::mcp_namespace::verified_employee_from_env(home_dir);
    let resolve_ns = |p: &crate::mcp_auth::Principal| {
        crate::mcp_namespace::resolve_for_caller(
            p,
            crate::mcp_namespace::CallerIdentity {
                verified_agent: verified_agent.as_deref(),
                client_is_agent: crate::mcp_namespace::client_is_agent(home_dir, &p.client_id),
            },
        )
    };
    let ns_ctx = resolve_ns(&principal)
        .map_err(|e| DuDuClawError::Gateway(format!("MCP namespace resolution failed: {e}")))?;

    // RFC-22 P1-10: Distinguish API key owner (`client_id`, used for namespace
    // isolation) from the actual calling agent (`caller_agent`, taken from
    // DUDUCLAW_AGENT_ID injected by per-agent .mcp.json). Without showing both,
    // observers reading the boot log mistakenly conclude all sub-agents act as
    // `claude-desktop` (the API key owner). Audit log (tool_calls.jsonl) was
    // already correct; only the boot log was misleading.
    tracing::info!(
        client_id = %principal.client_id,
        caller_agent = %default_agent,
        namespace = %ns_ctx.write_namespace,
        is_external = principal.is_external,
        "MCP server authenticated"
    );

    // ── McpDispatcher 初始化（W20-P1 Phase 2A）───────────────────
    // Wraps rate limiter, daily quota, odoo, memory and all tool handlers.
    // All three transports (stdio, HTTP, SSE) share this dispatcher.
    let dispatcher = crate::mcp_dispatch::McpDispatcher::new(
        home_dir.to_path_buf(),
        http.clone(),
        std::sync::Arc::new(memory),
        default_agent.clone(),
        // RFC-21 §2: per-agent Odoo connector pool (lazy — slot populated on
        // first odoo_connect call for the calling agent).
        std::sync::Arc::new(crate::odoo_pool::OdooConnectorPool::default()),
        crate::mcp_rate_limit::RateLimiter::new(),
        crate::mcp_memory_quota::DailyQuota::new(),
    );

    let workflow_session =
        super::workflow_operation::WorkflowSession::from_env(home_dir, &default_agent, &principal)
            .map_err(|e| DuDuClawError::Gateway(format!("workflow session rejected: {e}")))?;
    let workflow_available = workflow_session.is_some();
    let dispatcher = dispatcher.with_workflow_session(workflow_session);

    // ── RFC-23 redaction layer init ─────────────────────────────
    // None ⇒ pipeline not enabled in config.toml (the normal zero-overhead
    // path). An Err means the operator DID enable it and it failed to
    // initialise — spec §10.2 makes that fatal: an MCP server that keeps
    // serving tool results unredacted after the operator asked for redaction
    // is the exact leak the pipeline exists to prevent. Refuse to serve.
    let redaction_layer = match crate::mcp_redaction::McpRedactionLayer::try_init(
        home_dir,
        &default_agent,
    ) {
        Ok(opt) => {
            if let Some(ref layer) = opt {
                tracing::info!(
                    agent = %layer.agent_id,
                    session = %layer.session_id,
                    rules = layer.manager.engine().rule_count(),
                    "MCP redaction layer enabled"
                );
            }
            opt
        }
        Err(e) => {
            tracing::error!(
                error = %e,
                "MCP redaction layer failed to init — refusing to serve \
                 (config.toml [redaction] enabled = true). Fix the redaction \
                 config or disable it explicitly."
            );
            return Err(DuDuClawError::Gateway(format!(
                "redaction is enabled but failed to initialise; refusing to start the MCP server without it: {e}"
            )));
        }
    };

    // P2-4: attach the egress layer to the dispatcher so the "secret in-use"
    // decision + result redaction run inside the shared choke point
    // (`dispatch_tool_call`) for every transport — stdio (here), HTTP and SSE.
    // Previously this was wrapped manually around the stdio call only, leaving
    // HTTP/SSE uncovered. `None` ⇒ redaction disabled (zero-overhead skip).
    let dispatcher = dispatcher.with_redaction(redaction_layer.map(std::sync::Arc::new));

    let stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    // O7: `AsyncBufReadExt::lines()` rather than a reused `String` +
    // `read_line`, because the read now sits in a `tokio::select!` against the
    // watcher below. `read_line` is **not** cancellation safe — when the other
    // branch wins, whatever it had already consumed is gone, which on this
    // stream means a truncated JSON-RPC frame and a corrupted session.
    // `Lines::next_line` is cancel safe: partial data is carried over to the
    // next invocation.
    let mut lines = BufReader::new(stdin).lines();

    // ── O7: tools/list_changed watcher ───────────────────────────────────
    // `tools/list` is filtered by the caller's live capabilities, so a grant
    // minted mid-session (PORTICO `capability_request`, a goal-loop kickoff),
    // an operator editing `agent.toml`, or a dashboard `agent_update` changes
    // what this caller may see. `last_advertised` holds the set we actually
    // sent; `None` means no `tools/list` has been answered yet and there is
    // therefore nothing to invalidate. The watcher compares SETS rather than
    // file mtimes, so a touched-but-unchanged config emits nothing and a WAL
    // write we cannot stat still gets noticed.
    let mut last_advertised: Option<Vec<String>> = None;
    // Protocol era bookkeeping (MCP 2026-07-28, see `crate::mcp_protocol`).
    // A legacy client (`initialize`) gets the unsolicited `list_changed`; a
    // modern client only gets it on a `subscriptions/listen` it opened, keyed
    // by that request's id. A process that has seen neither keeps the old
    // unsolicited behaviour.
    let mut legacy_initialized = false;
    let mut modern_seen = false;
    let mut tool_subscriptions: Vec<Value> = Vec::new();
    let mut watch = tokio::time::interval(TOOLS_LIST_WATCH_INTERVAL);
    watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // First tick completes immediately; burn it so the watcher does not fire
    // before the loop has served anything.
    watch.tick().await;

    loop {
        let line = tokio::select! {
            next = lines.next_line() => {
                match next.map_err(|e| {
                    DuDuClawError::Gateway(format!("Failed to read from stdin: {e}"))
                })? {
                    Some(l) => l,
                    None => {
                        // EOF — client disconnected
                        info!("MCP server: stdin closed, shutting down");
                        break;
                    }
                }
            }
            _ = watch.tick() => {
                if let Some(previous) = last_advertised.as_ref()
                    && let Ok(p) =
                        crate::mcp_auth::authenticate_from_env_cached(home_dir, &auth_cache)
                {
                    let current: Vec<String> =
                        visible_tool_names(&p, home_dir, &default_agent)
                            .await
                            .into_iter()
                            .map(str::to_string)
                            .collect();
                    if &current != previous {
                        tracing::info!(
                            before = previous.len(),
                            after = current.len(),
                            "MCP tools/list changed — notifying client"
                        );
                        last_advertised = Some(current);
                        if legacy_initialized || !modern_seen {
                            write_response(&mut stdout, &tools_list_changed_notification())
                                .await?;
                        }
                        for sub in &tool_subscriptions {
                            write_response(
                                &mut stdout,
                                &crate::mcp_protocol::tagged_tools_list_changed(sub),
                            )
                            .await?;
                        }
                    }
                }
                continue;
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        // Redact any API keys from the raw line before it touches any log output.
        let redacted_line = crate::mcp_redact::redact(trimmed);
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(v) => v,
            Err(e) => {
                warn!(line = %redacted_line, "MCP server: invalid JSON: {e}");
                let err = jsonrpc_error(&Value::Null, -32700, "Parse error");
                write_response(&mut stdout, &err).await?;
                continue;
            }
        };

        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let method = request.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = request.get("params").cloned().unwrap_or(Value::Null);

        // A JSON-RPC notification (no `id`) is never answered, known or not.
        if request.get("id").is_none() {
            if method == "notifications/cancelled" {
                // Ends a modern subscription; after it nothing more is sent
                // for that id. Other requests here finish synchronously, so
                // there is nothing else to stop.
                if let Some(rid) = params.get("requestId") {
                    tool_subscriptions.retain(|s| s != rid);
                }
            }
            continue;
        }

        // `initialize` only exists in the legacy era; anything else carrying
        // the modern `_meta` revision is served statelessly (2026-07-28).
        let era = if method == "initialize" {
            crate::mcp_protocol::RequestEra::Legacy
        } else {
            crate::mcp_protocol::request_era(&params)
        };
        let modern = match era {
            crate::mcp_protocol::RequestEra::Unsupported(requested) => {
                let err = crate::mcp_protocol::unsupported_version_error(&id, &requested);
                write_response(&mut stdout, &err).await?;
                continue;
            }
            crate::mcp_protocol::RequestEra::Modern(_) => true,
            crate::mcp_protocol::RequestEra::Legacy => false,
        };
        if modern {
            modern_seen = true;
            if let Some(problem) = crate::mcp_protocol::modern_meta_problem(&params) {
                let err = crate::mcp_protocol::modernize_response(jsonrpc_error(
                    &id, -32602, &problem,
                ));
                write_response(&mut stdout, &err).await?;
                continue;
            }
        }
        let capabilities = || {
            let mut caps = stdio_capabilities();
            if workflow_available {
                caps["experimental"] = serde_json::json!({"duduclaw_workflow":{"version":1}});
            }
            caps
        };

        let response = match (method, modern) {
            ("initialize", _) => {
                legacy_initialized = true;
                let mut response = handle_initialize(&id, &params);
                response["result"]["capabilities"] = capabilities();
                response
            }
            // Removed in 2026-07-28; still part of every legacy revision.
            ("ping", false) => jsonrpc_response(&id, serde_json::json!({})),
            ("server/discover", true) => jsonrpc_response(
                &id,
                crate::mcp_protocol::discover_result(capabilities()),
            ),
            ("subscriptions/listen", true) => {
                // Acknowledge first (spec MUST), then deliver tagged
                // `list_changed` until the client cancels. No response is
                // written now: the request stays open.
                let wants = crate::mcp_protocol::wants_tools_list_changed(&params);
                if wants
                    && !tool_subscriptions.contains(&id)
                    && tool_subscriptions.len() >= crate::mcp_protocol::MAX_SUBSCRIPTIONS
                {
                    let err = crate::mcp_protocol::modernize_response(jsonrpc_error(
                        &id,
                        -32000,
                        &format!(
                            "at most {} open subscriptions per process",
                            crate::mcp_protocol::MAX_SUBSCRIPTIONS
                        ),
                    ));
                    write_response(&mut stdout, &err).await?;
                    continue;
                }
                // A subscriber the watcher cannot authenticate would never be
                // compared against, so refuse instead of acknowledging.
                let baseline = if wants {
                    match crate::mcp_auth::authenticate_from_env_cached(home_dir, &auth_cache) {
                        Ok(p) => Some(
                            visible_tool_names(&p, home_dir, &default_agent)
                                .await
                                .into_iter()
                                .map(str::to_string)
                                .collect::<Vec<_>>(),
                        ),
                        Err(e) => {
                            let err = crate::mcp_protocol::modernize_response(jsonrpc_error(
                                &id,
                                -32003,
                                &format!("MCP authentication failed: {e}"),
                            ));
                            write_response(&mut stdout, &err).await?;
                            continue;
                        }
                    }
                } else {
                    None
                };
                let ack = crate::mcp_protocol::subscription_ack(&id, wants);
                write_response(&mut stdout, &ack).await?;
                if !wants {
                    // Nothing this server offers was asked for: end the
                    // subscription at once instead of leaving it open.
                    write_response(&mut stdout, &crate::mcp_protocol::subscription_complete(&id))
                        .await?;
                    continue;
                }
                if last_advertised.is_none() {
                    last_advertised = baseline;
                }
                if !tool_subscriptions.contains(&id) {
                    tool_subscriptions.push(id.clone());
                }
                continue;
            }
            ("server/discover" | "subscriptions/listen", false) => jsonrpc_error(
                &id,
                -32602,
                &format!(
                    "{method} needs _meta[\"{}\"] (MCP 2026-07-28)",
                    crate::mcp_protocol::META_PROTOCOL_VERSION
                ),
            ),
            ("tools/list", _) => {
                // Gap (a): re-authenticate per call instead of reusing the
                // boot-time `principal` — a revoked/rescoped key must stop
                // (or start) seeing tools without a restart.
                match crate::mcp_auth::authenticate_from_env_cached(home_dir, &auth_cache) {
                    Ok(p) => {
                        let resp =
                            handle_tools_list_for_agent(&id, &p, home_dir, &default_agent).await;
                        // O7: remember exactly what we advertised — read back
                        // off the response we are about to send, not from a
                        // second evaluation, so the watcher above compares
                        // against the real wire content and the hot path pays
                        // for one pass, not two.
                        last_advertised = Some(advertised_tool_names(&resp));
                        if modern {
                            crate::mcp_protocol::with_cache_hints(
                                resp,
                                crate::mcp_protocol::TOOLS_LIST_TTL_MS,
                            )
                        } else {
                            resp
                        }
                    }
                    Err(e) => {
                        jsonrpc_error(&id, -32003, &format!("MCP authentication failed: {e}"))
                    }
                }
            }
            ("tools/call", _) => {
                // W20-P1 Phase 2A + P2-4: delegate to McpDispatcher, which now
                // enforces the full pipeline — including RFC-23 egress ("secret
                // in-use") arg-restoration and result redaction — inside the one
                // shared choke point. The former manual egress wrapping around
                // this call was removed so stdio / HTTP / SSE stay identical.
                //
                // Gap (a): re-authenticate per call (mtime-cached — see
                // `auth_cache` above) instead of reusing the boot-time
                // `principal` for the whole life of this long-running
                // subprocess.
                match crate::mcp_auth::authenticate_from_env_cached(home_dir, &auth_cache) {
                    Ok(p) => match resolve_ns(&p) {
                        Ok(ns) => dispatcher.dispatch_tool_call(&p, &ns, &params, &id).await,
                        Err(e) => jsonrpc_error(
                            &id,
                            -32003,
                            &format!("MCP namespace resolution failed: {e}"),
                        ),
                    },
                    Err(e) => {
                        jsonrpc_error(&id, -32003, &format!("MCP authentication failed: {e}"))
                    }
                }
            }
            _ => jsonrpc_error(&id, -32601, &format!("Method not found: {method}")),
        };
        let response = if modern {
            crate::mcp_protocol::modernize_response(response)
        } else {
            response
        };

        write_response(&mut stdout, &response).await?;
    }

    Ok(())
}

/// The boot-time authentication failure message. A missing key is the first
/// thing a developer arriving from an MCP directory listing hits, so it names
/// the one command that issues one.
pub(crate) fn startup_auth_error(e: &crate::mcp_auth::AuthError) -> String {
    match e {
        crate::mcp_auth::AuthError::MissingKey => format!(
            "MCP authentication failed: {e}. Run: duduclaw mcp init --client claude-code"
        ),
        _ => format!("MCP authentication failed: {e}"),
    }
}

#[cfg(test)]
mod startup_auth_error_tests {
    use super::startup_auth_error;
    use crate::mcp_auth::AuthError;

    #[test]
    fn missing_key_names_the_init_command() {
        assert_eq!(
            startup_auth_error(&AuthError::MissingKey),
            "MCP authentication failed: DUDUCLAW_MCP_API_KEY environment variable not set. \
             Run: duduclaw mcp init --client claude-code"
        );
    }

    #[test]
    fn other_errors_keep_their_text() {
        assert_eq!(
            startup_auth_error(&AuthError::UnknownKey),
            "MCP authentication failed: API key not found in registry"
        );
    }
}
