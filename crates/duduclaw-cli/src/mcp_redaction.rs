//! MCP-layer integration of the RFC-23 redaction pipeline.
//!
//! Two concerns:
//!
//! 1. **Outgoing (tool result → LLM)**: the whole tool result `Value` goes
//!    through [`RedactionPipeline::redact_value`] with the call's
//!    `(tool_name, arguments)` as context. That runs the structured field
//!    rules (which see JSON *keys*, so a customer name with no recognisable
//!    pattern still gets masked) and then the text-pattern rules over every
//!    string leaf. The vault stores `(agent_id, session_id, token)` keyed on
//!    the values so the gateway's channel-reply layer can later restore them.
//!
//! 2. **Incoming (tool args restoration)**: before a tool is executed,
//!    arguments that contain `<REDACT:...>` tokens are decided by
//!    [`EgressEvaluator`]. Whitelisted tools get real values; non-whitelisted
//!    tools (or args containing hallucinated tokens) are denied.
//!
//! Both paths key off two env vars set by the gateway when it spawns the
//! Claude CLI subprocess: `DUDUCLAW_AGENT_ID` and `DUDUCLAW_SESSION_ID`.
//! If either is missing the integration falls back to a sensible default
//! (default agent / "mcp-session") but cross-layer restoration may not
//! work end-to-end in that case.

use std::sync::Arc;

use duduclaw_redaction::{EgressDecision, RedactionConfig, RedactionManager, RestoreScope};
use serde_json::Value;

/// Per-MCP-server-process redaction state.
///
/// Built once at server startup from `config.toml [redaction]`. `None` ⇒
/// pipeline disabled at this layer (zero overhead path).
pub struct McpRedactionLayer {
    pub manager: Arc<RedactionManager>,
    pub agent_id: String,
    pub session_id: String,
}

impl McpRedactionLayer {
    /// Try to build the layer. Returns `Ok(None)` when redaction is not
    /// enabled in `config.toml` — that's the normal "off" path and not
    /// an error. Returns `Err` only if the config explicitly enabled the
    /// pipeline but it failed to initialise — in which case the caller
    /// should fail-closed (refuse to start the MCP server) or log loudly.
    pub fn try_init(
        home_dir: &std::path::Path,
        default_agent: &str,
    ) -> Result<Option<Self>, duduclaw_redaction::RedactionError> {
        let cfg_path = home_dir.join("config.toml");
        // Fail-closed on a config we cannot read the truth out of. A malformed
        // `[redaction]` block used to collapse (via `.ok()`) into "no config" →
        // "not enabled" → `Ok(None)`, so the server came up serving tool
        // results unredacted while the operator's config said `enabled = true`.
        // Only two things may yield `Ok(None)`: no config.toml at all, and a
        // config that parses and says `enabled = false`.
        let raw = match std::fs::read_to_string(&cfg_path) {
            Ok(s) => Some(s),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(duduclaw_redaction::RedactionError::config(format!(
                    "cannot read {}: {e}",
                    cfg_path.display()
                )));
            }
        };
        let Some(raw) = raw else {
            return Ok(None);
        };

        #[derive(serde::Deserialize)]
        struct Wrap {
            #[serde(default)]
            redaction: RedactionConfig,
        }
        let rcfg = toml::from_str::<Wrap>(&raw)
            .map_err(|e| {
                duduclaw_redaction::RedactionError::config(format!(
                    "{} has a malformed [redaction] block: {e}",
                    cfg_path.display()
                ))
            })?
            .redaction;

        if !rcfg.enabled {
            return Ok(None);
        }

        let paths = duduclaw_redaction::ManagerPaths::under_home(home_dir);
        let manager = Arc::new(RedactionManager::open(rcfg, paths)?);

        let agent_id = std::env::var(duduclaw_core::ENV_AGENT_ID)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| default_agent.to_string());
        let session_id = std::env::var(duduclaw_core::ENV_TRUST_SESSION_ID)
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "mcp-session".to_string());

        Ok(Some(Self {
            manager,
            agent_id,
            session_id,
        }))
    }

    /// Apply redaction to a tool-call result Value.
    ///
    /// `args` is the call's `arguments` object; structured field rules use it
    /// to decide which model a generic tool like `odoo_search` just returned.
    /// Tokens hit the shared vault keyed on `(self.agent_id, self.session_id)`
    /// so the channel-reply layer can restore them when this turn's final text
    /// reaches the user.
    ///
    /// Thin wrapper over the free function [`redact_tool_result_with`] with
    /// this layer's env-derived agent / session — the stdio serve loop path.
    /// `McpDispatcher` calls the free function directly with the authenticated
    /// `principal.client_id`, so both transports share one implementation
    /// (P2-4: egress pushed to a single choke point, no logic fork).
    pub fn redact_tool_result(&self, tool_name: &str, value: &mut Value, args: Option<&Value>) {
        redact_tool_result_with(
            &self.manager,
            tool_name,
            value,
            &self.agent_id,
            &self.session_id,
            args,
        );
    }

    /// Decide what to do with a tool call whose arguments may contain
    /// `<REDACT:...>` tokens.
    ///
    /// Thin wrapper over the free function [`decide_tool_args_with`] with this
    /// layer's env-derived agent / session (see [`Self::redact_tool_result`]
    /// for why the two paths share one implementation).
    pub fn decide_tool_args(&self, tool_name: &str, args: &Value) -> EgressDecision {
        decide_tool_args_with(&self.manager, tool_name, args, &self.agent_id, &self.session_id)
    }

    /// Quick scan: does any string in this Value contain a token-shaped
    /// substring? Used as a hot-path optimisation so we only invoke
    /// `decide_tool_args` when there's actually something to restore.
    pub fn args_contain_tokens(args: &Value) -> bool {
        match args {
            Value::String(s) => s.contains(duduclaw_redaction::token::TOKEN_PREFIX),
            Value::Array(arr) => arr.iter().any(Self::args_contain_tokens),
            Value::Object(map) => map.values().any(Self::args_contain_tokens),
            _ => false,
        }
    }
}

/// Read the operator-granted redaction scopes from the environment.
///
/// `DUDUCLAW_REDACTION_SCOPES` is comma-separated (e.g. `FinanceRead,CrmRead`).
/// Empty / unset ⇒ no extra scopes. `RedactionAdmin` bypasses every per-token
/// `RestoreScope`. Kept as env (not per-call) because it is an operator policy
/// knob, identical whether the call arrives over stdio or HTTP/SSE.
fn redaction_scopes_from_env() -> Vec<String> {
    std::env::var("DUDUCLAW_REDACTION_SCOPES")
        .ok()
        .map(|raw| {
            raw.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Egress decision for a tool call whose arguments may carry `<REDACT:...>`
/// tokens — the pure form that takes an explicit `(manager, agent_id,
/// session_id)` instead of reading them off a [`McpRedactionLayer`].
///
/// Shared by the stdio serve loop (via [`McpRedactionLayer::decide_tool_args`],
/// which passes its env-derived identity) and by `McpDispatcher` (which passes
/// the authenticated `principal.client_id` — more accurate than the env var).
/// Fail-closed (I5): a redaction error resolves to `Deny`, never a silent
/// passthrough.
///
/// C3: the caller is always modelled as the *agent*, never the channel
/// end-user (`owner`), so Owner-scoped PII is never exfiltrated to external
/// tools. Operators widen this via `DUDUCLAW_REDACTION_SCOPES`.
pub fn decide_tool_args_with(
    manager: &RedactionManager,
    tool_name: &str,
    args: &Value,
    agent_id: &str,
    session_id: &str,
) -> EgressDecision {
    let caller = duduclaw_redaction::Caller::agent(agent_id.to_string(), redaction_scopes_from_env());
    manager
        .decide_tool_call(tool_name, args, agent_id, Some(session_id), &caller)
        .unwrap_or_else(|e| {
            tracing::error!(
                target: "duduclaw_cli::mcp_redaction",
                error = %e,
                "decide_tool_args failed; denying"
            );
            EgressDecision::Deny {
                reason: format!("redaction error: {e}"),
                tokens_seen: 0,
            }
        })
}

/// Placeholder substituted for a tool result the pipeline could not redact.
pub const REDACTION_FAILED_PLACEHOLDER: &str = "[redaction failed — value withheld]";

/// Redact a tool-call result — the pure form taking an explicit
/// `(manager, agent_id, session_id)`.
///
/// Shared by the stdio serve loop (via [`McpRedactionLayer::redact_tool_result`])
/// and `McpDispatcher`. Vault writes are keyed on `(agent_id, session_id)` so
/// the channel-reply layer can restore the same tokens later.
///
/// `args` carries the call's `arguments` object so structured field rules can
/// resolve which model the result belongs to (`match_args`). Passing `None`
/// only disables rules that declare an argument gate; everything else, the
/// text rules included, is unaffected.
///
/// Fail-closed (spec §10.2): every failure on this path — the pipeline failing
/// to build (unreadable key directory, unusable agent key) as much as a
/// vault-write abort mid-redaction — replaces the whole value with a
/// placeholder. Both used to differ: a build failure warned and let the raw
/// result through untouched, which is precisely the leak redaction exists to
/// prevent, and it was invisible to the operator because the tool still
/// "worked".
pub fn redact_tool_result_with(
    manager: &RedactionManager,
    tool_name: &str,
    value: &mut Value,
    agent_id: &str,
    session_id: &str,
    args: Option<&Value>,
) {
    let pipeline = match manager.pipeline(agent_id, Some(session_id.to_string())) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(
                target: "duduclaw_cli::mcp_redaction",
                error = %e,
                agent = %agent_id,
                tool = %tool_name,
                "redact_tool_result: pipeline build failed; withholding the whole result"
            );
            *value = Value::String(REDACTION_FAILED_PLACEHOLDER.to_string());
            return;
        }
    };

    let ctx = duduclaw_redaction::ToolContext { tool_name, args };
    if let Err(e) = pipeline.redact_value(value, &ctx) {
        tracing::error!(
            target: "duduclaw_cli::mcp_redaction",
            error = %e,
            agent = %agent_id,
            tool = %tool_name,
            "redact_tool_result: redact failed; withholding the whole result"
        );
        *value = Value::String(REDACTION_FAILED_PLACEHOLDER.to_string());
    }
}

/// Convenience: produce a JSON-RPC error for a denied tool call.
pub fn egress_deny_response(id: &Value, tool: &str, reason: &str, tokens_seen: usize) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32007,
            "message": format!(
                "egress denied for '{tool}': {reason} (tokens_seen={tokens_seen})"
            ),
            "data": {
                "kind": "redaction_egress_deny",
                "tool": tool,
                "tokens_seen": tokens_seen,
            }
        }
    })
}

// silence unused-import warning when RestoreScope is referenced only in tests
#[allow(dead_code)]
fn _scope_marker(_s: &RestoreScope) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a manager with one `db_field` rule over `res.partner.name`.
    fn db_field_manager(home: &std::path::Path) -> RedactionManager {
        let mut cfg = RedactionConfig::default();
        cfg.enabled = true;
        cfg.rules.insert(
            "customer_master".to_string(),
            duduclaw_redaction::RuleSpec {
                id: "customer_master".into(),
                category: "DB_FIELD".into(),
                restore_scope: RestoreScope::Owner,
                priority: 70,
                cross_session_stable: false,
                apply_to_system_prompt: false,
                enabled: true,
                kind: duduclaw_redaction::RuleKind::DbField {
                    source: Some("odoo".into()),
                    connector: None,
                    fields: vec!["res.partner.name".into()],
                },
            },
        );
        RedactionManager::open(cfg, duduclaw_redaction::ManagerPaths::under_home(home)).unwrap()
    }

    #[test]
    fn tool_args_reach_the_structured_match_args_gate() {
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = db_field_manager(tmp.path());

        let rows = serde_json::json!([{"id": 7, "name": "王小明"}]);
        let right = serde_json::json!({"model": "res.partner", "limit": "20"});
        let wrong = serde_json::json!({"model": "crm.lead"});

        // Matching model ⇒ the field rule fires and the name is tokenised.
        let mut value = rows.clone();
        redact_tool_result_with(
            &manager,
            "odoo_search",
            &mut value,
            "agnes",
            "s1",
            Some(&right),
        );
        assert!(
            value[0]["name"].as_str().unwrap().starts_with("<REDACT:DB_FIELD:"),
            "args must reach match_args: {value}"
        );
        assert_eq!(value[0]["id"], serde_json::json!(7));

        // Different model ⇒ the gate blocks it.
        let mut value = rows.clone();
        redact_tool_result_with(
            &manager,
            "odoo_search",
            &mut value,
            "agnes",
            "s1",
            Some(&wrong),
        );
        assert_eq!(value[0]["name"], serde_json::json!("王小明"));

        // No args at all ⇒ the gate is unsatisfiable, same outcome.
        let mut value = rows;
        redact_tool_result_with(&manager, "odoo_search", &mut value, "agnes", "s1", None);
        assert_eq!(value[0]["name"], serde_json::json!("王小明"));
    }

    #[test]
    fn structured_rule_reaches_into_json_in_text_results() {
        // The shape the odoo_* tools actually return: records pretty-printed
        // into `content[0].text`.
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = db_field_manager(tmp.path());

        let rows = serde_json::json!([{"id": 7, "name": "王小明"}]);
        let mut value = serde_json::json!({
            "content": [{"type": "text", "text": serde_json::to_string_pretty(&rows).unwrap()}]
        });
        let args = serde_json::json!({"model": "res.partner"});
        redact_tool_result_with(
            &manager,
            "odoo_search",
            &mut value,
            "agnes",
            "s1",
            Some(&args),
        );
        let text = value["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("王小明"), "{text}");
        assert!(text.contains("<REDACT:DB_FIELD:"), "{text}");
    }

    #[test]
    fn pipeline_build_failure_withholds_the_whole_result() {
        // §10.2: a pipeline that cannot be built must never degrade to
        // passthrough. Force the failure by replacing the key directory (which
        // `RedactionManager::open` created) with a regular file, so the next
        // uncached agent's `load_or_generate` cannot `create_dir_all` it.
        let tmp = tempfile::TempDir::new().unwrap();
        let manager = db_field_manager(tmp.path());

        let key_dir = tmp.path().join("redaction").join("keys");
        std::fs::remove_dir_all(&key_dir).unwrap();
        std::fs::write(&key_dir, b"not a directory").unwrap();

        let mut value = serde_json::json!([{"id": 7, "name": "王小明"}]);
        let args = serde_json::json!({"model": "res.partner"});
        redact_tool_result_with(
            &manager,
            "odoo_search",
            &mut value,
            // An agent whose key has never been cached, so the build really runs.
            "never-seen-agent",
            "s1",
            Some(&args),
        );

        assert_eq!(
            value,
            Value::String(REDACTION_FAILED_PLACEHOLDER.to_string()),
            "a failed pipeline build must withhold the result, not pass it through"
        );
    }

    /// Write a `config.toml` into `home` and run `try_init` against it.
    fn try_init_with_config(
        home: &std::path::Path,
        body: &str,
    ) -> Result<Option<McpRedactionLayer>, duduclaw_redaction::RedactionError> {
        std::fs::write(home.join("config.toml"), body).unwrap();
        McpRedactionLayer::try_init(home, "agnes")
    }

    #[test]
    fn malformed_redaction_config_is_an_error_not_a_silent_disable() {
        // The live defect: a `[redaction]` block that fails to deserialise used
        // to collapse into `Ok(None)` — "redaction not enabled" — and the MCP
        // server came up serving tool results in the clear.
        let tmp = tempfile::TempDir::new().unwrap();
        let err = try_init_with_config(
            tmp.path(),
            r#"
[redaction]
enabled = true

[redaction.rules.known_people]
type = "identity"
category = "PERSON"
priority = "not-a-number"
"#,
        )
        .err()
        .expect("a malformed [redaction] block must fail, never disable silently");
        assert!(
            err.to_string().contains("malformed [redaction] block"),
            "{err}"
        );
    }

    #[test]
    fn disabled_redaction_config_still_returns_ok_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let got = try_init_with_config(
            tmp.path(),
            "[redaction]
enabled = false
",
        )
        .expect("a well-formed disabled config is not an error");
        assert!(got.is_none(), "disabled ⇒ the zero-overhead path");
    }

    #[test]
    fn missing_config_file_still_returns_ok_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let got = McpRedactionLayer::try_init(tmp.path(), "agnes")
            .expect("no config.toml is the fresh-install case, not an error");
        assert!(got.is_none());
    }

    #[test]
    fn ai_pii_profile_without_the_model_refuses_to_serve() {
        // §13.4 fail-closed. The MCP server resolves the model under
        // `DUDUCLAW_HOME`; with no model installed the `ner` rule must fail to
        // compile, which surfaces here as `try_init` erroring — and the caller
        // (mcp.rs) then refuses to start rather than serving unredacted tool
        // results under a rule set the operator believes is protecting them.
        let tmp = tempfile::TempDir::new().unwrap();
        let err = try_init_with_config(
            tmp.path(),
            r#"
[redaction]
enabled = true
profiles = ["ai_pii"]
"#,
        )
        .err()
        .expect("an uninstalled NER model must fail the layer, never disable it silently");
        let msg = err.to_string();
        assert!(
            msg.contains("ai_pii") || msg.contains("模型"),
            "the error must name the model problem: {msg}"
        );
    }

    #[test]
    fn ai_pii_model_dir_is_resolved_under_duduclaw_home() {
        // Not a behaviour test of the model — a wiring test: whichever home
        // the MCP server was given is the home the model is looked for in.
        let tmp = tempfile::TempDir::new().unwrap();
        let paths = duduclaw_redaction::ManagerPaths::under_home(tmp.path());
        let dirs = paths.ner_dirs.expect("under_home must set the model dirs");
        assert_eq!(
            dirs.model_dir,
            tmp.path().join("models").join("privacy-filter")
        );
        assert_eq!(dirs.ort_lib_root, tmp.path().join("lib").join("onnxruntime"));
    }

    #[test]
    fn bare_identity_rule_now_parses_and_initialises() {
        // End-to-end of the two fixes: `source` may be omitted, and the block
        // parses instead of vanishing. With a real people directory the layer
        // comes up enabled.
        let tmp = tempfile::TempDir::new().unwrap();
        let people = tmp.path().join("shared/wiki/identity/people");
        std::fs::create_dir_all(&people).unwrap();
        std::fs::write(
            people.join("ruby.md"),
            "---\nperson_id: p1\ndisplay_name: Ruby Lin\n---\n",
        )
        .unwrap();

        let layer = try_init_with_config(
            tmp.path(),
            r#"
[redaction]
enabled = true

[redaction.rules.known_people]
type = "identity"
category = "PERSON"
"#,
        )
        .expect("bare identity rule must initialise")
        .expect("enabled ⇒ Some(layer)");
        assert_eq!(layer.manager.engine().rule_count(), 1);
    }

    #[test]
    fn args_contain_tokens_detects_at_depth() {
        let no = serde_json::json!({"a": "plain", "b": ["x"]});
        let yes = serde_json::json!({"a": "plain", "b": ["<REDACT:E:abcdef01>"]});
        assert!(!McpRedactionLayer::args_contain_tokens(&no));
        assert!(McpRedactionLayer::args_contain_tokens(&yes));
    }

    #[test]
    fn deny_response_has_expected_shape() {
        let r = egress_deny_response(
            &serde_json::json!(7),
            "web_fetch",
            "not whitelisted",
            2,
        );
        assert_eq!(r["error"]["code"], -32007);
        assert_eq!(r["error"]["data"]["tool"], "web_fetch");
        assert_eq!(r["error"]["data"]["tokens_seen"], 2);
    }
}
