//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Interactive CLI login ("Dashboard 一鍵登入") ──────────────────────
    //
    // Drives each AI CLI's native login command in a PTY, streams output to the
    // dashboard as `auth.cli_login.output` events, relays input back, and
    // reports terminal status. See `cli_auth.rs` for the per-CLI registry and
    // the local-callback-vs-remote feasibility constraint.

    pub(crate) async fn handle_cli_login_start(&self, params: Value) -> WsFrame {
        let runtime_str = params.get("runtime").and_then(|v| v.as_str()).unwrap_or("");
        if runtime_str.is_empty() {
            return WsFrame::error_response(
                "",
                &format!(
                    "runtime is required ({})",
                    duduclaw_core::types::RuntimeType::valid_values()
                ),
            );
        }
        // WP-B: REFUSE an unknown runtime. `RuntimeType::parse` used to map any
        // unrecognised string to Claude, so `{"runtime": "kimi"}` against a
        // build that did not know `kimi` silently ran `claude setup-token` and
        // showed the user Anthropic's login screen. The accepted list comes
        // from the catalog, so it can never go stale.
        let Some(runtime) = duduclaw_core::types::RuntimeType::parse(runtime_str) else {
            return WsFrame::error_response(
                "",
                &format!(
                    "unknown runtime '{runtime_str}' ({})",
                    duduclaw_core::types::RuntimeType::valid_values()
                ),
            );
        };
        let spec = match crate::cli_auth::spec_for(runtime) {
            Some(s) => s,
            None => {
                // API-key-only runtimes (openai_compat, qwen, vibe): naming the
                // key variable turns a dead end into an actionable answer.
                let key_hint = runtime
                    .spec()
                    .auth
                    .api_key_env
                    .map(|e| format!(" — set {e} instead"))
                    .unwrap_or_default();
                return WsFrame::error_response(
                    "",
                    &format!("'{runtime_str}' has no interactive login (use an API key{key_hint})"),
                );
            }
        };

        let session_id = uuid::Uuid::new_v4().simple().to_string();
        let session = match crate::cli_auth::AuthSession::spawn(
            session_id.clone(),
            runtime,
            std::collections::HashMap::new(),
        ) {
            Ok(s) => s,
            Err(crate::cli_auth::AuthError::NotInstalled) => {
                return WsFrame::error_response(
                    "",
                    &format!("{runtime_str} CLI not installed on this host"),
                );
            }
            Err(e) => return WsFrame::error_response("", &format!("failed to start login: {e}")),
        };
        let program = session.program.clone();

        self.cli_auth_sessions
            .write()
            .await
            .insert(session_id.clone(), session.clone());

        // Forward PTY output + terminal status to the dashboard event stream.
        if let Some(tx) = self.event_tx.read().await.clone() {
            let mut rx = session.subscribe();
            let sess = session.clone();
            let sid = session_id.clone();
            tokio::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                let emit = |event: &str, payload: Value| {
                    let frame = WsFrame::Event {
                        event: event.to_string(),
                        payload,
                        seq: None,
                        state_version: None,
                    };
                    let _ = tx.send(serde_json::to_string(&frame).unwrap_or_default());
                };
                loop {
                    match tokio::time::timeout(std::time::Duration::from_millis(500), rx.recv())
                        .await
                    {
                        Ok(Ok(bytes)) => {
                            let data = String::from_utf8_lossy(&bytes).to_string();
                            emit(
                                "auth.cli_login.output",
                                json!({"session_id": sid, "data": data}),
                            );
                        }
                        Ok(Err(RecvError::Lagged(_))) => continue,
                        Ok(Err(RecvError::Closed)) => break,
                        Err(_) => {} // timeout → fall through to status check
                    }
                    if sess.status().is_terminal() {
                        emit(
                            "auth.cli_login.status",
                            json!({"session_id": sid, "status": sess.status().as_str()}),
                        );
                        break;
                    }
                }
            });
        }

        WsFrame::ok_response(
            "",
            json!({
                "session_id": session_id,
                "runtime": runtime_str,
                "program": program,
                "remote_safe": spec.remote_safe,
                "hint": spec.hint,
                "status": "running",
            }),
        )
    }

    pub(crate) async fn handle_cli_login_input(&self, params: Value) -> WsFrame {
        let sid = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let data = params.get("data").and_then(|v| v.as_str()).unwrap_or("");
        // Clone the Arc and drop the map lock — we sleep between writes below and
        // must not hold the registry lock across the await.
        let session = {
            let sessions = self.cli_auth_sessions.read().await;
            match sessions.get(sid) {
                Some(s) => s.clone(),
                None => return WsFrame::error_response("", "login session not found"),
            }
        };
        tracing::info!(
            target: "cli_auth",
            session = %sid,
            bytes = data.len(),
            ends_cr = data.ends_with('\r'),
            ends_lf = data.ends_with('\n'),
            "cli_login input received"
        );

        // The login CLIs use an Ink masked-input prompt. A long pasted code that
        // arrives in the SAME write as its trailing Enter is treated as one paste
        // and the CR is swallowed into the field — the code never submits and the
        // dashboard spins forever (confirmed by PTY probe: code+CR together does
        // nothing; code, then a SEPARATE CR after a brief pause, submits). So
        // split: write the body, let Ink commit the paste, then send Enter (CR)
        // as a distinct keystroke. LF does not submit, so normalize the
        // terminator to CR.
        let body_and_term = match data.strip_suffix('\r').or_else(|| data.strip_suffix('\n')) {
            Some(body) => Some((body, "\r")),
            None => None,
        };
        let write_res = match body_and_term {
            Some((body, term)) => {
                let r1 = if body.is_empty() {
                    Ok(())
                } else {
                    session.write_input(body.as_bytes())
                };
                if r1.is_ok() {
                    tokio::time::sleep(std::time::Duration::from_millis(350)).await;
                    session.write_input(term.as_bytes())
                } else {
                    r1
                }
            }
            None => session.write_input(data.as_bytes()),
        };
        match write_res {
            Ok(()) => WsFrame::ok_response("", json!({"success": true})),
            Err(e) => WsFrame::error_response("", &format!("failed to send input: {e}")),
        }
    }

    pub(crate) async fn handle_cli_login_status(&self, params: Value) -> WsFrame {
        let sid = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let sessions = self.cli_auth_sessions.read().await;
        let Some(session) = sessions.get(sid) else {
            return WsFrame::error_response("", "login session not found");
        };
        WsFrame::ok_response(
            "",
            json!({
                "session_id": sid,
                "status": session.status().as_str(),
            }),
        )
    }

    pub(crate) async fn handle_cli_login_cancel(&self, params: Value) -> WsFrame {
        let sid = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Some(session) = self.cli_auth_sessions.write().await.remove(sid) {
            session.kill();
        }
        WsFrame::ok_response("", json!({"success": true}))
    }

    /// Register the account produced by a successful one-click login. `claude
    /// setup-token` only PRINTS its long-lived token (never persists it), so the
    /// PTY session scrapes it; this turns that token into a real `[[accounts]]`
    /// entry and refreshes the rotator so it shows up immediately. Idempotent-ish:
    /// each call makes a uniquely-named account. No-op (not an error) when the
    /// session didn't succeed or no token was captured.
    pub(crate) async fn handle_cli_login_finalize(&self, params: Value) -> WsFrame {
        let sid = params
            .get("session_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let session = {
            let sessions = self.cli_auth_sessions.read().await;
            match sessions.get(sid) {
                Some(s) => s.clone(),
                None => return WsFrame::error_response("", "login session not found"),
            }
        };
        if session.status() != crate::cli_auth::AuthStatus::Succeeded {
            return WsFrame::ok_response(
                "",
                json!({"registered": false, "reason": "login not succeeded"}),
            );
        }
        let Some(token) = session.captured_token() else {
            // Only `claude setup-token` ever PRINTS a token — every other CLI
            // (grok/codex/gemini) persists credentials to its own store, which
            // is exactly what the success_file watcher confirmed. For those,
            // "no scrapeable token" IS the expected success path: the runtime
            // reads the CLI store directly, no [[accounts]] entry is needed.
            // Report it as `cli_store` so the dashboard renders success, not a
            // warning. A tokenless CLAUDE login stays an anomaly (the scrape
            // failed) and keeps the old reason.
            if session.runtime != duduclaw_core::types::RuntimeType::Claude {
                let store = crate::cli_auth::spec_for(session.runtime)
                    .and_then(|s| s.success_file)
                    .map(|f| format!("~/{f}"))
                    .unwrap_or_else(|| "CLI 憑證儲存".to_string());
                return WsFrame::ok_response(
                    "",
                    json!({"registered": false, "reason": "cli_store", "store": store}),
                );
            }
            return WsFrame::ok_response(
                "",
                json!({"registered": false, "reason": "no token captured"}),
            );
        };

        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let runtime_key = session.runtime.as_str();
        let id = format!("{runtime_key}-oauth-{secs}");

        let add = json!({
            "id": id,
            "type": "oauth",
            "key": token,
            "priority": 1,
            "monthly_budget_cents": 0,
        });
        let res = self.handle_accounts_add(add).await;
        // Drop the cached rotator so accounts.list / budget_summary rebuild from
        // the just-written config and surface the new account immediately.
        crate::claude_runner::invalidate_rotator_cache().await;

        match res {
            WsFrame::Response { ok: true, .. } => {
                tracing::info!(target: "cli_auth", session = %sid, account = %id, "one-click login: account registered");
                WsFrame::ok_response("", json!({"registered": true, "account_id": id}))
            }
            other => other, // propagate the accounts.add error verbatim
        }
    }
}
