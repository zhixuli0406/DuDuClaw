//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Models ──────────────────────────────────────────────

    /// Detect which AI runtime CLIs are installed and whether Claude OAuth is
    /// available — drives the dashboard onboarding "choose your AI backend"
    /// step so we can flag detected vs. not-installed backends. Viewer-level:
    /// returns only presence booleans + subscription tier, never any secret.
    pub(crate) async fn handle_runtime_detect(&self) -> WsFrame {
        // `which_runtime_in_home` expects the OS USER home (`~`) — its
        // candidates are `~/.bun/bin`, `~/.nvm/...`, etc. Passing
        // `self.home_dir` (`~/.duduclaw`) here made every HOME-rooted install
        // invisible; only the fixed absolute paths (Homebrew) could ever hit.
        // PATH-first so terminal-launched gateways see what the user sees.
        let user_home = std::path::PathBuf::from(duduclaw_core::platform::home_dir());

        // WP-B: one loop over `runtime_catalog` replaces five hand-written
        // `which_*` probes. Every runtime gets a boolean keyed by its catalog
        // id, plus a `runtimes` array carrying the metadata the wizard needs to
        // render a row (display name, install channel, login method, ToS note)
        // without a second RPC or a duplicated table in the web client.
        let mut flags = serde_json::Map::new();
        let mut rows: Vec<Value> = Vec::new();
        let mut claude_bin: Option<String> = None;
        for spec in duduclaw_core::runtime_catalog::cli_specs() {
            let found = duduclaw_core::detect_runtime(spec.id, &user_home);
            if spec.id == "claude" {
                claude_bin = found.clone();
            }
            flags.insert(spec.id.to_string(), Value::Bool(found.is_some()));
            let cred_present = spec
                .auth
                .credential_paths
                .first()
                .map(|rel| Value::Bool(user_home.join(rel).exists()));
            rows.push(runtime_detect_row(spec, found.as_deref(), cred_present));
        }
        let (claude_oauth, claude_subscription) = detect_claude_oauth(claude_bin.as_deref()).await;

        // Legacy key kept verbatim: the dashboard onboarding wizard and the
        // OOBE shell both read `claude_cli`, and renaming it would break both
        // for zero gain. Every other runtime is keyed by its catalog id
        // (`codex`, `gemini`, `antigravity`, `grok`, … ) exactly as before.
        flags.insert("claude_cli".to_string(), Value::Bool(claude_bin.is_some()));
        flags.insert("claude_oauth".to_string(), Value::Bool(claude_oauth));
        flags.insert(
            "claude_subscription".to_string(),
            claude_subscription
                .map(Value::String)
                .unwrap_or(Value::Null),
        );
        flags.insert("runtimes".to_string(), Value::Array(rows));

        WsFrame::ok_response("", Value::Object(flags))
    }

    /// **WP2 / D16** — install a missing AI CLI on the user's behalf so the
    /// onboarding wizard never has to say "go open a terminal".
    ///
    /// Pure dispatch: all policy (the provider→command whitelist, platform
    /// support, prerequisite checks, concurrency guard, timeout, audit trail)
    /// lives in [`crate::runtime_install`]. The only accepted parameter is
    /// `provider`; an unrecognised value is an error, never a default.
    ///
    /// Progress streams as `runtime.install.output` / `runtime.install.status`
    /// events — the same shape the one-click login uses for
    /// `auth.cli_login.*`, so the dashboard reuses its transcript component.
    ///
    /// The caller's identity is threaded into the audit trail: running an
    /// installer mutates the host, so `security_audit.jsonl` must record which
    /// admin did it rather than a generic `"system"` actor.
    pub(crate) async fn handle_runtime_install(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let provider = params
            .get("provider")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let Some(spec) = crate::runtime_install::spec_for(provider) else {
            // Fail closed: unknown / malformed provider resolves to nothing.
            // The accepted list comes from the catalog, so it cannot go stale.
            return WsFrame::error_response(
                "",
                &format!(
                    "unsupported provider ({})",
                    duduclaw_core::runtime_catalog::id_list_pipe()
                ),
            );
        };
        let event_tx = self.event_tx.read().await.clone();
        let actor = crate::runtime_install::Actor::new(ctx.user_id.as_str(), ctx.email.as_str());
        let outcome =
            crate::runtime_install::start_install(spec, self.home_dir.clone(), actor, event_tx)
                .await;
        WsFrame::ok_response("", outcome.to_payload(spec))
    }

    /// List all available models (cloud + local GGUF files).
    ///
    /// Cloud models come from the [`crate::runtime_models`] discovery cache —
    /// probed live from each installed CLI / API on a 12h background refresh —
    /// **not** a hard-coded list. Each cloud entry carries `provider` / `source`
    /// / `fetched_at` so the UI can show "updated N ago" and flag entries whose
    /// `source` is `fallback` (live discovery failed → stale static list).
    pub(crate) async fn handle_models_list(&self) -> WsFrame {
        let cache = crate::runtime_models::load_or_refresh(&self.home_dir).await;
        self.build_models_response(&cache).await
    }

    /// Force a live re-probe of every provider, persist the cache, and return
    /// the fresh list. Authenticated (any logged-in dashboard user) — a manual
    /// "refresh" action, not a config change.
    pub(crate) async fn handle_models_refresh(&self) -> WsFrame {
        let cache = crate::runtime_models::refresh_and_save(&self.home_dir).await;
        self.build_models_response(&cache).await
    }

    /// Shared assembler: merge the deduped cloud discovery + local GGUF scan +
    /// `inference.toml` default into the `models.list` payload shape.
    pub(crate) async fn build_models_response(
        &self,
        cache: &crate::runtime_models::RuntimeModelsCache,
    ) -> WsFrame {
        // Cloud models: deduped across providers, each tagged provider/source/
        // fetched_at.
        let mut models = crate::runtime_models::merged_models(cache);

        // Local models: scan ~/.duduclaw/models/ for GGUF files
        let models_dir = self.home_dir.join("models");
        if let Ok(mut entries) = tokio::fs::read_dir(&models_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let size = entry.metadata().await.map(|m| m.len()).unwrap_or(0);
                let size_gb = size as f64 / (1024.0 * 1024.0 * 1024.0);
                models.push(json!({
                    "id": format!("local:{name}"),
                    "label": format!("{name} ({size_gb:.1}GB)"),
                    "type": "local",
                    "file": name,
                    "size_bytes": size,
                }));
            }
        }

        // Also read default_model from inference.toml if it exists
        let inf_path = self.home_dir.join("inference.toml");
        let default_model = if let Ok(content) = tokio::fs::read_to_string(&inf_path).await {
            content
                .parse::<toml::Table>()
                .ok()
                .and_then(|t| t.get("default_model")?.as_str().map(|s| s.to_string()))
        } else {
            None
        };

        WsFrame::ok_response(
            "",
            json!({
                "models": models,
                "default_local": default_model,
                "discovered_at": cache.fetched_at,
            }),
        )
    }
}

/// One `runtime.detect` row. Pure (no probing) so its shape is unit-testable.
pub(crate) fn runtime_detect_row(
    spec: &duduclaw_core::runtime_catalog::RuntimeSpec,
    found: Option<&str>,
    cred_present: Option<Value>,
) -> Value {
    let dep = spec.deprecation;
    json!({
        "id": spec.id,
        "display_name": spec.display_name,
        "binary": spec.binary,
        "installed": found.is_some(),
        "path": found,
        "install_channel": spec.install.kind(),
        "install_command": spec.install.command_display(),
        "login_method": spec.auth.login.kind(),
        "login_remote_safe": spec.auth.login.remote_safe(),
        "api_key_env": spec.auth.api_key_env,
        "credential_present": cred_present,
        "mcp": spec.mcp,
        "headless_verified": spec.verified,
        "vendor_url": spec.vendor_url,
        "tos_note": spec.auth.tos_note.map(|t| json!({
            "en": t.en, "zh-TW": t.zh_tw, "ja-JP": t.ja_jp,
        })),
        // R1 (2026-10): catalog deprecation window, so the dashboard
        // can label a saved deprecated runtime instead of offering it.
        "deprecated": dep.is_some(),
        "replacement": dep.map(|d| d.replacement),
        "remove_in": dep.map(|d| d.remove_in),
    })
}
