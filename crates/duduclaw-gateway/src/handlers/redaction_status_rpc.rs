//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── RFC-23 Redaction read-only RPCs ─────────────────────

    pub(crate) async fn handle_redaction_stats(&self) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response(
                "",
                json!({
                    "enabled": false,
                    "vault": { "total": 0, "active": 0, "expired": 0, "by_category": [] },
                    "rule_count": 0,
                }),
            );
        };
        match duduclaw_redaction::dashboard::handle_stats(&manager) {
            Ok(s) => match serde_json::to_value(&s) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize stats: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction stats: {e}")),
        }
    }

    pub(crate) async fn handle_redaction_recent_audit(&self, params: Value) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response("", json!({ "entries": [] }));
        };
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(50) as usize;
        let req = duduclaw_redaction::dashboard::RecentAuditRequest { limit };
        match duduclaw_redaction::dashboard::handle_recent_audit(&manager, req) {
            Ok(r) => match serde_json::to_value(&r) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize audit: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction audit: {e}")),
        }
    }

    pub(crate) async fn handle_redaction_override_status(&self) -> WsFrame {
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response(
                "",
                json!({
                    "active": false,
                    "banner": null,
                    "record": null,
                }),
            );
        };
        match duduclaw_redaction::dashboard::handle_override_status(&manager) {
            Ok(s) => match serde_json::to_value(&s) {
                Ok(v) => WsFrame::ok_response("", v),
                Err(e) => WsFrame::error_response("", &format!("serialize override: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction override: {e}")),
        }
    }

    pub(crate) async fn handle_redaction_policy_status(&self) -> WsFrame {
        // §12: the poison state rides on BOTH shapes — the manager-absent
        // fallback is exactly the case a poisoned boot lands in, so omitting it
        // there would hide the very failure this field exists to surface.
        let poison = self.get_redaction_poison().await;
        let Some(manager) = self.get_redaction_manager().await else {
            return WsFrame::ok_response(
                "",
                json!({
                    "config_enabled": false,
                    "vault_ttl_hours": 0,
                    "purge_after_expire_days": 0,
                    "rule_count": 0,
                    "override_active": false,
                    "poisoned": poison_wire(poison.as_ref()),
                }),
            );
        };
        match duduclaw_redaction::dashboard::handle_policy_status(&manager) {
            Ok(s) => match serde_json::to_value(&s) {
                Ok(mut v) => {
                    if let Some(obj) = v.as_object_mut() {
                        obj.insert("poisoned".into(), poison_wire(poison.as_ref()));
                    }
                    WsFrame::ok_response("", v)
                }
                Err(e) => WsFrame::error_response("", &format!("serialize policy: {e}")),
            },
            Err(e) => WsFrame::error_response("", &format!("redaction policy: {e}")),
        }
    }

    /// `redaction.dry_run` — run a pasted sample through the live pipeline
    /// (optionally plus unsaved draft rules) and report WHERE it would be
    /// tokenised.
    ///
    /// Params: `{ sample_json?: string, sample_text?: string,
    /// tool?: string (default "odoo_search"), args?: object (default {}),
    /// draft_rules?: [{ id, label?, category, kind, keywords?, pattern? }] }`.
    /// Response:
    /// `{ hits: [{ pointer, rule_id, category, token }], token_count, restored_ok }`.
    ///
    /// `sample_text` is the plain-text alternative to `sample_json` — it is
    /// wrapped as a JSON string value internally, so the wizard's「試一試」
    /// step can paste prose without hand-building JSON. `sample_json` wins
    /// when both are given.
    ///
    /// `draft_rules` previews rules that have NOT been saved. They are
    /// compiled into a candidate rule set for this call only — never written
    /// anywhere — layered on top of the live config's inline rules, so a draft
    /// sharing an id with an existing rule overrides it (the "edit preview"
    /// case). Events a draft produced carry the draft's own `rule_id`, so the
    /// UI can count "this rule · N hits"; every other rule reports exactly as
    /// it does today.
    ///
    /// The sample is wrapped exactly the way `duduclaw redaction verify` JSON
    /// mode wraps it (`{"content":[{"type":"text","text": pretty}]}`) so the
    /// embedded-JSON pass is exercised, and the response deliberately carries
    /// **no original values** — not even masked ones. The operator is checking
    /// coverage ("did `res.partner.street` get caught?"), which the pointer and
    /// rule id answer; echoing the PII back through the dashboard would defeat
    /// the feature it is verifying.
    pub(crate) async fn handle_redaction_dry_run(&self, params: Value) -> WsFrame {
        use duduclaw_redaction::{Caller, RestoreTarget, ToolContext};

        /// Cap on the pasted sample. Large enough for a realistic Odoo page,
        /// small enough that a paste cannot tie up the vault.
        const SAMPLE_MAX_BYTES: usize = 256 * 1024;
        const DRY_RUN_SESSION: &str = "dashboard-dry-run";

        // `sample_json` wins when both are present. `sample_text` is wrapped
        // as a JSON string value — the exact `JSON.stringify(text)` the
        // frontend would otherwise have had to build itself.
        let sample_owned: String = match params.get("sample_json").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => match params.get("sample_text").and_then(|v| v.as_str()) {
                Some(t) if !t.trim().is_empty() => {
                    match serde_json::to_string(&Value::String(t.to_string())) {
                        Ok(s) => s,
                        Err(e) => {
                            return WsFrame::error_response(
                                "",
                                &format!("cannot wrap sample_text: {e}"),
                            );
                        }
                    }
                }
                Some(_) => return WsFrame::error_response("", "sample_text is empty"),
                None => {
                    return WsFrame::error_response(
                        "",
                        "Missing 'sample_json' parameter (or 'sample_text')",
                    );
                }
            },
        };
        let sample = sample_owned.as_str();
        if sample.trim().is_empty() {
            return WsFrame::error_response("", "sample_json is empty");
        }
        if sample.len() > SAMPLE_MAX_BYTES {
            return WsFrame::error_response(
                "",
                &format!("sample_json too large (max {SAMPLE_MAX_BYTES} bytes)"),
            );
        }
        // Validated BEFORE anything else runs: an invalid draft is an error
        // for the whole call, never a partial run against the rest.
        let draft_specs = match crate::redaction_custom_rules::parse_draft_rules(&params) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let tool_name = params
            .get("tool")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("odoo_search")
            .to_string();
        let args = match params.get("args") {
            None | Some(Value::Null) => json!({}),
            Some(v) if v.is_object() => v.clone(),
            Some(_) => return WsFrame::error_response("", "args must be an object"),
        };
        let parsed: Value = match serde_json::from_str(sample) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response("", &format!("sample_json 不是合法 JSON：{e}"));
            }
        };

        // Fail closed with the state named: "disabled" and "poisoned" are very
        // different answers to "why did nothing get redacted?".
        let Some(live_manager) = self.get_redaction_manager().await else {
            let msg = match self.get_redaction_poison().await {
                Some(p) => format!(
                    "去識別化目前處於毒化狀態，無法試跑（設定修好後會自動恢復）：{}",
                    p.reason
                ),
                None => "去識別化未啟用，無法試跑。請先在本頁開啟保護並儲存。".to_string(),
            };
            return WsFrame::error_response("", &msg);
        };

        // With drafts, build a CANDIDATE manager from the on-disk config plus
        // the drafts as inline rules. Inline rules are resolved last and
        // override a profile rule with the same id, which is exactly the
        // "previewing an edit" semantics the wizard needs. Nothing is written:
        // the candidate lives only for this call.
        let manager = if draft_specs.is_empty() {
            live_manager
        } else {
            let table = self
                .read_config_table(&self.home_dir.join("config.toml"))
                .await;
            match self.build_dry_run_manager(&table, draft_specs) {
                Ok(m) => m,
                Err(e) => {
                    return WsFrame::error_response("", &format!("試跑規則無法編譯：{e}"));
                }
            }
        };

        let agent_id = self.resolve_dry_run_agent().await;
        let pipeline = match manager.pipeline(&agent_id, Some(DRY_RUN_SESSION.to_string())) {
            Ok(p) => p,
            Err(e) => return WsFrame::error_response("", &format!("pipeline build failed: {e}")),
        };

        let pretty = match serde_json::to_string_pretty(&parsed) {
            Ok(p) => p,
            Err(e) => {
                return WsFrame::error_response("", &format!("cannot re-serialise sample: {e}"));
            }
        };
        let mut wrapped = json!({ "content": [{ "type": "text", "text": pretty }] });
        let ctx = ToolContext {
            tool_name: &tool_name,
            args: Some(&args),
        };
        let tokens = match pipeline.redact_value(&mut wrapped, &ctx) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("redact_value failed: {e}")),
        };

        // Owner round-trip: proves each token is reversible for the caller who
        // is allowed to see it. The restored text stays local to this function.
        let redacted_text = match serde_json::to_string(&wrapped) {
            Ok(t) => t,
            Err(e) => return WsFrame::error_response("", &format!("cannot serialise result: {e}")),
        };
        let restored = pipeline
            .restore(
                &redacted_text,
                &Caller::owner(&agent_id),
                RestoreTarget::UserChannel,
            )
            .unwrap_or_default();

        let mut locations: Vec<(String, String)> = Vec::new();
        duduclaw_redaction::collect_token_locations(&wrapped, "", &mut locations);

        let mut hits: Vec<Value> = Vec::with_capacity(locations.len());
        let mut restored_ok = 0usize;
        for (pointer, token) in locations {
            let entry = manager
                .vault()
                .lookup_mapping(&token, &agent_id, Some(DRY_RUN_SESSION))
                .ok()
                .flatten();
            let (rule_id, category, reversible) = match entry {
                Some(e) => {
                    let original = e.original.unwrap_or_default();
                    let ok = !original.is_empty() && restored.contains(&original);
                    (e.rule_id, e.category, ok)
                }
                // A token in the output with no vault row is a real defect —
                // surface the row rather than dropping it.
                None => ("(not in vault)".to_string(), "?".to_string(), false),
            };
            if reversible {
                restored_ok += 1;
            }
            hits.push(json!({
                "pointer": pointer,
                "rule_id": rule_id,
                "category": category,
                "token": token,
            }));
        }

        WsFrame::ok_response(
            "",
            json!({
                "hits": hits,
                "token_count": tokens.len(),
                "restored_ok": restored_ok,
            }),
        )
    }

    /// Build a throwaway [`duduclaw_redaction::RedactionManager`] for one dry
    /// run: the on-disk `[redaction]` config plus `drafts` as inline rules.
    ///
    /// Drafts go into `RedactionConfig::rules`, the inline map that
    /// `resolve_rule_specs` walks LAST — so a draft sharing an id with a
    /// profile rule shadows it, which is what previewing an edit means. The
    /// candidate manager is dropped when the call returns; nothing about it
    /// reaches disk or the live pipeline.
    pub(crate) fn build_dry_run_manager(
        &self,
        table: &toml::Table,
        drafts: Vec<duduclaw_redaction::RuleSpec>,
    ) -> Result<Arc<duduclaw_redaction::RedactionManager>, String> {
        #[derive(serde::Deserialize)]
        struct Wrap {
            #[serde(default)]
            redaction: duduclaw_redaction::RedactionConfig,
        }
        let raw = toml::to_string(table).map_err(|e| format!("serialize config: {e}"))?;
        let mut cfg = toml::from_str::<Wrap>(&raw)
            .map_err(|e| format!("parse [redaction]: {e}"))?
            .redaction;
        for spec in drafts {
            cfg.rules.insert(spec.id.clone(), spec);
        }
        // A dry run must work even while the operator is still deciding
        // whether to turn protection on; the live-manager gate above already
        // answered "is redaction configured at all?".
        cfg.enabled = true;
        crate::redaction_integration::build_manager_from_home(&self.home_dir, cfg)
            .map_err(|e| e.to_string())
    }

    /// Agent identity used for a dashboard dry run: `[general] default_agent`
    /// when it names a valid id, else the first registered agent, else
    /// `"default"`. The id only picks which per-agent key salts the sample's
    /// tokens, but it still reaches a filename under `redaction/keys/`, so an
    /// invalid id is rejected rather than trusted.
    pub(crate) async fn resolve_dry_run_agent(&self) -> String {
        let table = self
            .read_config_table(&self.home_dir.join("config.toml"))
            .await;
        if let Some(a) = table
            .get("general")
            .and_then(|v| v.as_table())
            .and_then(|g| g.get("default_agent"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|a| duduclaw_core::is_valid_agent_id(a))
        {
            return a.to_string();
        }
        let reg = self.registry.read().await;
        let mut names: Vec<String> = reg
            .list()
            .iter()
            .map(|a| a.config.agent.name.clone())
            .filter(|n| duduclaw_core::is_valid_agent_id(n))
            .collect();
        names.sort();
        names
            .into_iter()
            .next()
            .unwrap_or_else(|| "default".to_string())
    }
}
