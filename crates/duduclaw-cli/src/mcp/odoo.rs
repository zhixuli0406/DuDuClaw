use super::*;

pub(crate) async fn handle_odoo_tool(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
) -> Value {
    use duduclaw_odoo::connector::OdooConnector;
    use duduclaw_odoo::models::{accounting, crm, inventory, sale};

    // odoo_connect doesn't require an existing connection
    if tool == "odoo_connect" {
        return handle_odoo_connect(home_dir, odoo, caller_agent).await;
    }

    if tool == "odoo_status" {
        return match odoo.is_connected(caller_agent).await {
            true => {
                // Probe the connector by triggering get_or_connect; it's
                // already cached so this is a hashmap lookup, not an HTTP
                // call. The decrypt closure is a never-called fallback.
                match odoo
                    .get_or_connect(caller_agent, |_: String| async {
                        Err::<String, String>("unreachable".into())
                    })
                    .await
                {
                    Ok(conn) => {
                        // Probe common.version: a long-lived connector may
                        // survive an Odoo upgrade between two status calls.
                        let live_version = conn.version().await.ok().and_then(|v| {
                            v.get("server_version")
                                .and_then(Value::as_str)
                                .filter(|s| !s.is_empty())
                                .map(duduclaw_odoo::schema::safe_version)
                        });
                        let mut s = conn.status();
                        s.version = live_version
                            .clone()
                            .unwrap_or_else(|| "unknown (live version probe failed)".into());
                        let key = odoo.pool_key(caller_agent).await;
                        let schema_line = schema_status_line(
                            home_dir,
                            &conn,
                            &key.1,
                            live_version.as_deref().unwrap_or("unknown"),
                        );
                        if live_version.is_some() {
                            duduclaw_odoo::schema::spawn_auto_scan(
                                conn.clone(),
                                home_dir.to_path_buf(),
                                conn.schema_profile(&key.1),
                            );
                        }
                        serde_json::json!({ "content": [{"type": "text", "text": odoo_status_text(&s, &key.0, &key.1, &schema_line)}]})
                    }
                    Err(e) => mcp_error(&format!("Odoo status: connector slot lost: {e}")),
                }
            }
            false => serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Odoo not connected for agent '{caller_agent}'. Call odoo_connect first."
                )}],
                "isError": true
            }),
        };
    }

    // RFC-21 §2 acceptance: defence-in-depth. Reject the call before any
    // HTTP round-trip leaves the process when `agent.toml [odoo]
    // .allowed_models` / `.allowed_actions` doesn't cover it.
    if let Some((verb, model)) = classify_odoo_call(tool, params) {
        // v1.68.0: the dashboard's 功能模組 switches (`[odoo] features_*`).
        if let Err(module) = odoo.feature_gate(&model).await {
            duduclaw_security::audit::append_tool_call(
                home_dir,
                caller_agent,
                tool,
                &format!("DENIED: {model}/{verb} — Odoo module '{module}' is switched off"),
                false,
            );
            return mcp_error(&format!(
                "Odoo feature_gate: module '{module}' is switched off in the dashboard (Odoo → 功能模組); \
                 '{model}' is not available."
            ));
        }
        let cfg = odoo.agent_override(caller_agent).await;
        if let Err(reason) = crate::odoo_pool::check_action_permission(cfg.as_ref(), verb, &model) {
            // Audit the policy denial so operators can spot misconfigured
            // agents without having to grep MCP logs.
            duduclaw_security::audit::append_tool_call(
                home_dir,
                caller_agent,
                tool,
                &format!("DENIED: {model}/{verb} — {reason}"),
                false,
            );
            return mcp_error(&format!("Odoo permission denied: {reason}"));
        }
    }

    // All other tools require an active per-agent connection. Use the
    // pool's cache fast-path; the decrypt closure is unreachable here
    // because cold-connect is owned by `handle_odoo_connect`.
    let conn_arc = match odoo
        .get_or_connect(caller_agent, |_: String| async {
            Err::<String, String>("Odoo connector not initialised — call odoo_connect first".into())
        })
        .await
    {
        Ok(c) => c,
        Err(_) => {
            return serde_json::json!({
                "content": [{"type": "text", "text": format!(
                    "Odoo not connected for agent '{caller_agent}'. Call odoo_connect first."
                )}],
                "isError": true
            });
        }
    };
    let conn: &OdooConnector = conn_arc.as_ref();
    // ── per-call audit attribution (RFC-21 §2 acceptance) ────────────────
    // Surface caller_agent + profile + tool + params summary to
    // tool_calls.jsonl so the audit trail attributes Odoo activity to a
    // specific agent rather than to the global admin user.
    let _audit_profile = odoo.pool_key(caller_agent).await.1;
    // A: effective block-list opt-out for this agent (per-agent override, else
    // global). Empty ⇒ the built-in security block list is fully in force.
    let unblock_models = odoo.unblock_models(caller_agent).await;

    let result: std::result::Result<String, String> = match tool {
        "odoo_schema_overview" => {
            let profile = conn.schema_profile(&_audit_profile);
            match duduclaw_odoo::schema::load_snapshot(home_dir, &profile) {
                Ok(Some(snapshot)) if snapshot.connection_identity == conn.connection_identity() => {
                    let cfg = odoo.agent_override(caller_agent).await;
                    let mut permitted = std::collections::HashSet::new();
                    for model in &snapshot.models {
                        if odoo.feature_gate(&model.model).await.is_ok()
                            && crate::odoo_pool::check_action_permission(cfg.as_ref(), "read", &model.model).is_ok()
                        {
                            permitted.insert(model.model.clone());
                        }
                    }
                    let model = params.get("model").and_then(Value::as_str).unwrap_or("");
                    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
                    Ok(schema_overview(&snapshot, &permitted, model, query).to_string())
                }
                Ok(Some(_)) => Err("Odoo schema snapshot belongs to a different connection; ask an admin to press 重新掃描 Odoo 結構 in the dashboard".into()),
                Ok(None) => Err("No Odoo schema snapshot yet. Ask an admin to press 重新掃描 Odoo 結構 in the dashboard".into()),
                Err(error) => Err(error),
            }
        }
        "odoo_crm_leads" => {
            let stage = params.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params
                .get("limit")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            let mut domain = vec![];
            if !stage.is_empty() {
                domain.push(serde_json::json!(["stage_id.name", "ilike", stage]));
            }
            match conn
                .search_read("crm.lead", domain, crm::CRM_LEAD_FIELDS, limit)
                .await
            {
                Ok(data) => {
                    let leads: Vec<crm::CrmLead> = data
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(crm::map_crm_lead)
                        .collect();
                    Ok(serde_json::to_string_pretty(&leads).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_crm_create_lead" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return mcp_error("name is required");
            }
            let mut vals = serde_json::json!({"name": name, "type": "lead"});
            if let Some(v) = params.get("contact_name").and_then(|v| v.as_str()) {
                vals["contact_name"] = serde_json::json!(v);
            }
            if let Some(v) = params.get("email").and_then(|v| v.as_str()) {
                vals["email_from"] = serde_json::json!(v);
            }
            if let Some(v) = params.get("phone").and_then(|v| v.as_str()) {
                vals["phone"] = serde_json::json!(v);
            }
            if let Some(v) = params
                .get("expected_revenue")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
            {
                vals["expected_revenue"] = serde_json::json!(v);
            }
            match conn.create("crm.lead", vals).await {
                Ok(id) => Ok(format!("CRM lead created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_crm_update_stage" => {
            let lead_id = params
                .get("lead_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let stage_name = params
                .get("stage_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if lead_id == 0 || stage_name.is_empty() {
                return mcp_error("lead_id and stage_name are required");
            }
            // Find stage ID by name
            match conn
                .search_read(
                    "crm.stage",
                    vec![serde_json::json!(["name", "ilike", stage_name])],
                    &["id", "name"],
                    1,
                )
                .await
            {
                Ok(stages) => {
                    let stage_id = stages
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|s| s["id"].as_i64())
                        .unwrap_or(0);
                    if stage_id == 0 {
                        return mcp_error(&format!("Stage '{stage_name}' not found"));
                    }
                    match conn
                        .write(
                            "crm.lead",
                            &[lead_id],
                            serde_json::json!({"stage_id": stage_id}),
                        )
                        .await
                    {
                        Ok(_) => Ok(format!("Lead {lead_id} moved to stage '{stage_name}'")),
                        Err(e) => Err(e),
                    }
                }
                Err(e) => Err(e),
            }
        }
        "odoo_sale_orders" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params
                .get("limit")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            let mut domain = vec![];
            if !status.is_empty() {
                domain.push(serde_json::json!(["state", "=", status]));
            }
            match conn
                .search_read("sale.order", domain, sale::SALE_ORDER_FIELDS, limit)
                .await
            {
                Ok(data) => {
                    let orders: Vec<sale::SaleOrder> = data
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(sale::map_sale_order)
                        .collect();
                    Ok(serde_json::to_string_pretty(&orders).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_sale_create_quotation" => {
            let partner_id = params
                .get("partner_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let product_id = params
                .get("product_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            let qty = params
                .get("quantity")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(1.0);
            if partner_id == 0 || product_id == 0 {
                return mcp_error("partner_id and product_id are required");
            }
            let vals = serde_json::json!({
                "partner_id": partner_id,
                "order_line": [[0, 0, {"product_id": product_id, "product_uom_qty": qty}]],
            });
            match conn.create("sale.order", vals).await {
                Ok(id) => Ok(format!("Quotation created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_sale_confirm" => {
            let order_id = params
                .get("order_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            if order_id == 0 {
                return mcp_error("order_id is required");
            }
            match conn
                .execute_kw(
                    "sale.order",
                    "action_confirm",
                    vec![serde_json::json!([order_id])],
                    serde_json::json!({}),
                )
                .await
            {
                Ok(_) => Ok(format!("Order {order_id} confirmed")),
                Err(e) => Err(e),
            }
        }
        "odoo_inventory_products" => {
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params
                .get("limit")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            let mut domain = vec![serde_json::json!(["detailed_type", "=", "product"])];
            if !query.is_empty() {
                domain.push(serde_json::json!(["name", "ilike", query]));
            }
            match conn
                .search_read("product.product", domain, inventory::PRODUCT_FIELDS, limit)
                .await
            {
                Ok(data) => {
                    let products: Vec<inventory::Product> = data
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(inventory::map_product)
                        .collect();
                    Ok(serde_json::to_string_pretty(&products).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_inventory_check" => {
            let product_id = params
                .get("product_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            if product_id == 0 {
                return mcp_error("product_id is required");
            }
            let domain = vec![serde_json::json!(["product_id", "=", product_id])];
            match conn
                .search_read("stock.quant", domain, inventory::STOCK_QUANT_FIELDS, 10)
                .await
            {
                Ok(data) => {
                    let quants: Vec<inventory::StockQuant> = data
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(inventory::map_stock_quant)
                        .collect();
                    Ok(serde_json::to_string_pretty(&quants).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_invoice_list" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params
                .get("limit")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            let mut domain = vec![serde_json::json!([
                "move_type",
                "in",
                ["out_invoice", "in_invoice"]
            ])];
            if !status.is_empty() {
                match status {
                    "paid" => domain.push(serde_json::json!(["payment_state", "=", "paid"])),
                    "draft" => domain.push(serde_json::json!(["state", "=", "draft"])),
                    "posted" => domain.push(serde_json::json!(["state", "=", "posted"])),
                    _ => {}
                }
            }
            match conn
                .search_read("account.move", domain, accounting::INVOICE_FIELDS, limit)
                .await
            {
                Ok(data) => {
                    let invoices: Vec<accounting::Invoice> = data
                        .as_array()
                        .unwrap_or(&vec![])
                        .iter()
                        .map(accounting::map_invoice)
                        .collect();
                    Ok(serde_json::to_string_pretty(&invoices).unwrap_or_default())
                }
                Err(e) => Err(e),
            }
        }
        "odoo_payment_status" => {
            let invoice_id = params
                .get("invoice_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            if invoice_id == 0 {
                return mcp_error("invoice_id is required");
            }
            match conn
                .search_read(
                    "account.move",
                    vec![serde_json::json!(["id", "=", invoice_id])],
                    accounting::INVOICE_FIELDS,
                    1,
                )
                .await
            {
                Ok(data) => {
                    let inv = data
                        .as_array()
                        .and_then(|a| a.first())
                        .map(accounting::map_invoice);
                    match inv {
                        Some(i) => Ok(serde_json::to_string_pretty(&i).unwrap_or_default()),
                        None => Err(format!("Invoice {invoice_id} not found")),
                    }
                }
                Err(e) => Err(e),
            }
        }
        "odoo_search" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() {
                return mcp_error("model is required");
            }
            if let Err(denial) = duduclaw_odoo::check_blocklist(
                model,
                duduclaw_odoo::AccessKind::Read,
                &unblock_models,
            ) {
                return mcp_error(&blocklist_denial_message(model, denial));
            }
            let domain_str = params
                .get("domain")
                .and_then(|v| v.as_str())
                .unwrap_or("[]");
            let domain: Vec<Value> = serde_json::from_str(domain_str).unwrap_or_default();
            let fields_str = params
                .get("fields")
                .and_then(|v| v.as_str())
                .unwrap_or("id,name");
            let fields: Vec<&str> = fields_str.split(',').map(|s| s.trim()).collect();
            let limit = params
                .get("limit")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse().ok())
                .unwrap_or(20usize);
            match conn.search_read(model, domain, &fields, limit).await {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_execute" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            let method = params.get("method").and_then(|v| v.as_str()).unwrap_or("");
            let ids_str = params.get("ids").and_then(|v| v.as_str()).unwrap_or("[]");
            if model.is_empty() || method.is_empty() {
                return mcp_error("model and method are required");
            }
            // A: block-list gate honours per-agent unblock_models. Classify the
            // method into read vs mutate so a system table (ir.model/…) stays
            // read-only even when unblocked (fail closed on unknown methods).
            let access_kind = if odoo_method_to_verb(method) == "read" {
                duduclaw_odoo::AccessKind::Read
            } else {
                duduclaw_odoo::AccessKind::Mutate
            };
            if let Err(denial) = duduclaw_odoo::check_blocklist(model, access_kind, &unblock_models)
            {
                return mcp_error(&blocklist_denial_message(model, denial));
            }

            // Whitelist safe Odoo methods — block dangerous ones like unlink, write on sensitive models (MCP-H6)
            const BLOCKED_METHODS: &[&str] = &[
                "unlink",
                "uninstall",
                "uninstall_hook",
                "init",
                "_auto_init",
                "_register_hook",
                "signal_workflow",
                "execute_import",
            ];
            if BLOCKED_METHODS.contains(&method) {
                return mcp_error(&method_denial_message(method));
            }
            let ids: Vec<Value> = serde_json::from_str(ids_str).unwrap_or_default();
            match conn
                .execute_kw(
                    model,
                    method,
                    vec![serde_json::json!(ids)],
                    serde_json::json!({}),
                )
                .await
            {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_report" => {
            let report_name = params
                .get("report_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let record_id = params
                .get("record_id")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            if report_name.is_empty() || record_id == 0 {
                return mcp_error("report_name and record_id are required");
            }
            // Reports use a special render method
            match conn
                .execute_kw(
                    "ir.actions.report",
                    "render_qweb_pdf",
                    vec![
                        serde_json::json!(report_name),
                        serde_json::json!([record_id]),
                    ],
                    serde_json::json!({}),
                )
                .await
            {
                Ok(_) => Ok(format!(
                    "Report '{report_name}' generated for record {record_id}. Download from Odoo."
                )),
                Err(e) => Err(format!("Report generation failed: {e}")),
            }
        }
        "odoo_partner_search" => {
            // Safe customer lookup — read-only over res.partner with a fixed
            // non-sensitive field projection (no bank/tax data). Bypasses the
            // generic block list because the field set is hard-coded; still
            // gated by OdooRead scope + per-agent read permission upstream.
            let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
            let limit = params
                .get("limit")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(10)
                .clamp(1, 40);
            match conn.partner_search(query, limit).await {
                Ok(data) => Ok(serde_json::to_string_pretty(&data).unwrap_or_default()),
                Err(e) => Err(e),
            }
        }
        "odoo_schema_fields" => {
            // Field metadata for one known model (fields_get). Metadata only —
            // no business rows — so it is safe on any model; per-agent
            // allowed_models still applies via classify_odoo_call.
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() {
                return mcp_error("model is required");
            }
            match conn.schema_fields_report(model, 200).await {
                Ok(fields) => Ok(schema_fields_metadata(&fields).to_string()),
                Err(e) => Err(e),
            }
        }
        _ => Err(format!("Unknown Odoo tool: {tool}")),
    };

    // RFC-21 §2 acceptance: per-call audit attribution. tool_calls.jsonl
    // now carries the originating agent + profile + outcome so Odoo
    // activity can be traced to the agent that triggered it (and not the
    // shared admin user inside Odoo's own audit log).
    let params_summary = format!(
        "profile={}; tool={}; ok={}",
        _audit_profile,
        tool,
        result.is_ok(),
    );
    // B3b: `odoo_*` deliberately stays out of the central `is_state_changing`
    // dispatch gate (see the comment on that list — it audits itself here to
    // avoid double-logging), so it needs its own `result_text` capture. Odoo
    // reads (partner search, schema fields, ...) return real business data
    // in `Ok(text)` — exactly the kind of evidence B3's grounding pre-check
    // needs, and it was silently excluded before this change. Error text is
    // captured too (masked/capped inside the helper); `check_grounded`
    // already excludes `is_error` evidence, so this never lets a failed
    // Odoo call masquerade as grounding.
    let result_text: &str = match &result {
        Ok(text) => text.as_str(),
        Err(e) => e.as_str(),
    };
    duduclaw_security::audit::append_tool_call_with_input(
        home_dir,
        caller_agent,
        tool,
        &params_summary,
        result.is_ok(),
        None,
        Some(result_text),
    );

    match result {
        Ok(text) => serde_json::json!({ "content": [{"type": "text", "text": text}] }),
        Err(e) => {
            serde_json::json!({ "content": [{"type": "text", "text": format!("Odoo error: {e}")}], "isError": true })
        }
    }
}

/// Heuristic mapping of `(tool, params)` to `(verb, model)` so the per-agent
/// `allowed_actions` / `allowed_models` filter can run before any HTTP call
/// reaches Odoo. Returns `None` for `odoo_status` / `odoo_connect` (those
/// need no model permission).
pub(crate) fn classify_odoo_call(tool: &str, params: &Value) -> Option<(&'static str, String)> {
    match tool {
        "odoo_crm_leads" => Some(("search", "crm.lead".into())),
        "odoo_crm_create_lead" => Some(("create", "crm.lead".into())),
        "odoo_crm_update_stage" => Some(("write", "crm.lead".into())),
        "odoo_sale_orders" => Some(("search", "sale.order".into())),
        "odoo_sale_create_quotation" => Some(("create", "sale.order".into())),
        "odoo_sale_confirm" => Some(("execute", "sale.order".into())),
        "odoo_inventory_products" => Some(("search", "product.product".into())),
        "odoo_inventory_check" => Some(("search", "stock.quant".into())),
        "odoo_invoice_list" | "odoo_payment_status" => Some(("search", "account.move".into())),
        "odoo_partner_search" => Some(("read", "res.partner".into())),
        "odoo_schema_fields" | "odoo_schema_overview" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() {
                None
            } else {
                Some(("read", model.to_string()))
            }
        }
        "odoo_search" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() {
                None
            } else {
                Some(("search", model.to_string()))
            }
        }
        "odoo_execute" => {
            let model = params.get("model").and_then(|v| v.as_str()).unwrap_or("");
            if model.is_empty() {
                return None;
            }
            // HS8: derive the real verb from `params["method"]` instead of
            // hard-coding "execute". Otherwise `allowed_actions=["execute"]`
            // (or even ["read","search","execute"]) would silently authorise a
            // `method:"write"` / `method:"unlink"` / `action_archive` call that
            // the per-agent action filter is supposed to block.
            let method = params.get("method").and_then(|v| v.as_str()).unwrap_or("");
            Some((odoo_method_to_verb(method), model.to_string()))
        }
        "odoo_report" => {
            let name = params
                .get("report_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if name.is_empty() {
                None
            } else {
                Some(("execute", name.to_string()))
            }
        }
        _ => None,
    }
}

/// Render a user-facing message for a block-list denial. Points the operator at
/// the right knob (unblock_models) without leaking internal paths, and keeps the
/// system-metadata read-only case distinct from the ordinary security default.
pub(crate) fn blocklist_denial_message(model: &str, denial: duduclaw_odoo::BlockDenial) -> String {
    match denial {
        duduclaw_odoo::BlockDenial::SecurityDefault => format!(
            "Odoo block list gate: model '{model}' is blocked by a security default. An admin can unlock it by adding \
             it to unblock_models in the agent's Odoo settings (dashboard: Odoo → 解鎖的 Models)."
        ),
        duduclaw_odoo::BlockDenial::SystemReadOnly => format!(
            "Odoo block list gate (system read-only): model '{model}' may be read with unblock_models in the agent's Odoo settings; writing or deleting is always refused, even when unblocked."
        ),
    }
}

pub(crate) fn method_denial_message(method: &str) -> String {
    format!(
        "Odoo method safety gate: method '{method}' is blocked. This fixed safety rule cannot be changed by allowed_actions or unblock_models in the agent's Odoo settings."
    )
}

/// Map an Odoo ORM `method` name to the coarse verb used by the per-agent
/// `allowed_actions` filter. CRUD methods map to `read`/`create`/`write`/
/// `unlink`; `action_*` / `button_*` workflow methods map to the qualified
/// `action_<name>` form (so `allowed_actions` can name them explicitly);
/// everything else is treated as a generic `execute`.
pub(crate) fn odoo_method_to_verb(method: &str) -> &'static str {
    match method {
        "read" | "search" | "search_read" | "search_count" | "fields_get" | "name_get"
        | "name_search" | "default_get" | "read_group" => "read",
        "create" | "copy" => "create",
        "write" | "update" => "write",
        "unlink" => "unlink",
        // Workflow / archive / state-change buttons are the dangerous ones the
        // reviewer flagged (e.g. action_archive). Classify them under the
        // qualified `action_*` family so they don't slip through as `execute`.
        m if m.starts_with("action_") || m.starts_with("button_") || m.starts_with("toggle_") => {
            "action"
        }
        _ => "execute",
    }
}

/// Connect to Odoo using `config.toml [odoo]` overlaid with the caller's
/// `agent.toml [odoo]` block (when present). RFC-21 §2: each agent ends up
/// with its own per-pool slot so cross-project credential leakage and
/// audit-log mis-attribution are eliminated at the system layer.
pub(crate) async fn handle_odoo_connect(
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
) -> Value {
    use duduclaw_odoo::AgentOdooConfig;

    // ── 1. Reload global config from disk so operator edits land on next connect ─
    let config_path = home_dir.join("config.toml");
    let content = match tokio::fs::read_to_string(&config_path).await {
        Ok(c) => c,
        Err(e) => return mcp_error(&format!("Cannot read config.toml: {e}")),
    };
    let global_table: toml::Table = match content.parse() {
        Ok(t) => t,
        Err(e) => return mcp_error(&format!("Invalid config.toml: {e}")),
    };
    let global_cfg = duduclaw_odoo::OdooConfig::from_toml(&global_table);
    if !global_cfg.is_configured() {
        // Name the file actually read: the dashboard's `odoo.configure` writes
        // the gateway's config.toml, and if this process resolved a different
        // DUDUCLAW_HOME, "configured in the dashboard" and "configured here"
        // are different files — the path is the diagnosis.
        return mcp_error(&format!(
            "Odoo not configured: no [odoo] url/db in {}. Configure Odoo from the dashboard, or add the [odoo] section there. If the dashboard already shows Odoo configured, its gateway may be using a different DUDUCLAW_HOME than this process.",
            config_path.display()
        ));
    }
    odoo.set_global(global_cfg.clone()).await;

    // ── 2. Reload caller agent's [odoo] override if their agent.toml has one ──
    // W3-3b (a): the caller may be an `eph-*` role member, whose agent.toml
    // lives under `agents/.ephemeral/<id>/`.
    let agent_toml_path = caller_agent_dir(home_dir, caller_agent).join("agent.toml");
    // A missing agent.toml or a missing [odoo] block ⇒ global config. A block
    // (or file) that cannot be read ⇒ refuse, never fall back to the global
    // server; any override registered earlier is dropped.
    let override_cfg: Option<AgentOdooConfig> =
        match tokio::fs::read_to_string(&agent_toml_path).await {
            Ok(raw) => match AgentOdooConfig::parse_agent_toml(&raw) {
                Ok(cfg) => cfg,
                Err(msg) => {
                    odoo.unregister_agent(caller_agent).await;
                    return mcp_error(&msg);
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            // Present but unreadable (permissions, I/O): refuse, never fall
            // back to the global server.
            Err(_) => {
                odoo.unregister_agent(caller_agent).await;
                return mcp_error(duduclaw_odoo::ODOO_UNREADABLE);
            }
        };
    match &override_cfg {
        Some(cfg) => odoo.register_agent(caller_agent, cfg.clone()).await,
        // No (or unusable) override any more: drop one registered earlier so
        // the agent falls back to the global server, as `odoo.agent_test` does.
        None => odoo.unregister_agent(caller_agent).await,
    }

    // ── 3. Force a fresh handshake — the previous slot, if any, may have ───
    //       been authed against a stale config.
    odoo.disconnect(caller_agent).await;

    // ── 4. Cold connect via the pool — credential merge + decrypt happen ──
    //       inside `OdooConnectorPool::get_or_connect` using the resolver
    //       state we just registered.
    let home_dir_owned = home_dir.to_path_buf();
    let connector = match odoo
        .get_or_connect(caller_agent, move |cred: String| {
            // WP-8C: previously a hand-rolled "starts_with secret:// ? resolve
            // via secret_manager : decrypt as AES ciphertext" branch that
            // duplicated `SecretRef`. `api_key_enc` / `password_enc` is a
            // single field that may hold ciphertext, a raw `secret://…`
            // reference, or (unresolvable ciphertext, not a reference)
            // legacy plaintext — exactly the shape `SecretRef::from_single`
            // classifies. Resolution (including the network-backend fetch)
            // is async, so this closure stays async.
            let home = home_dir_owned.clone();
            async move {
                let sm_cfg = duduclaw_security::secret_manager::SecretManagerConfig::load_from_home(&home).await;
                SecretRef::from_single(&cred)
                    .resolve(&sm_cfg, &home)
                    .await
                    .map(Secret::expose_owned)
                    .ok_or_else(|| {
                        "Odoo credential not found, could not be decrypted, or its secret:// reference could not be resolved".to_string()
                    })
            }
        })
        .await
    {
        Ok(c) => c,
        Err(e) => return mcp_error(&format!("Odoo connection failed: {e}")),
    };

    let status = connector.status();
    let key = odoo.pool_key(caller_agent).await;
    let schema_profile = connector.schema_profile(&key.1);
    duduclaw_odoo::schema::spawn_auto_scan(
        connector.clone(),
        home_dir.to_path_buf(),
        schema_profile,
    );
    serde_json::json!({
        "content": [{"type": "text", "text": odoo_connect_text(&status, &key.0, &key.1)}]
    })
}

pub(crate) fn odoo_status_text(
    status: &duduclaw_odoo::connector::OdooStatus, agent: &str, profile: &str, schema_line: &str,
) -> String {
    let mut s = status.clone();
    s.sanitize_metadata();
    format!(
        "Odoo connected (agent={}, profile={}): {} ({})\nEdition: {}\nVersion: {}\nUser ID: {}\nEE modules: {}\n{}\nuntrusted_odoo_metadata: true\n{}",
        agent, profile, s.url, s.db, s.edition, s.version,
        s.uid.map(|u| u.to_string()).unwrap_or("-".into()),
        if s.ee_modules.is_empty() { "none".to_string() } else { s.ee_modules.join(", ") },
        schema_line, duduclaw_odoo::schema::UNTRUSTED_METADATA_NOTE,
    )
}

pub(crate) fn odoo_connect_text(
    status: &duduclaw_odoo::connector::OdooStatus, agent: &str, profile: &str,
) -> String {
    let mut status = status.clone();
    status.sanitize_metadata();
    format!(
        "Connected to Odoo {} ({}) — {} v{}\n  agent={}, profile={}\nuntrusted_odoo_metadata: true\n{}",
        status.url, status.db, status.edition, status.version,
        agent, profile, duduclaw_odoo::schema::UNTRUSTED_METADATA_NOTE,
    )
}

fn schema_status_line(
    home: &Path,
    conn: &duduclaw_odoo::OdooConnector,
    profile: &str,
    live_version: &str,
) -> String {
    let profile = conn.schema_profile(profile);
    let mut line = match duduclaw_odoo::schema::load_snapshot(home, &profile) {
        Ok(Some(snapshot)) if snapshot.connection_identity == conn.connection_identity() => {
            snapshot.status_line(live_version)
        }
        Ok(None) => "Schema snapshot: not scanned yet; ask an admin to press 重新掃描 Odoo 結構 in the dashboard".into(),
        _ => "Schema snapshot: unavailable; re-scan recommended".into(),
    };
    if duduclaw_odoo::schema::load_scan_status(home, &profile).last_error.is_some() {
        line.push_str("; last schema scan failed; check Odoo connectivity and metadata permissions, then re-scan");
    }
    line
}

/// The authorization set is computed before projection or searching. Never
/// return inventory counts or relations from models outside that set.
pub(crate) fn schema_overview(
    snapshot: &duduclaw_odoo::schema::SchemaSnapshot,
    permitted: &std::collections::HashSet<String>,
    model_filter: &str,
    query: &str,
) -> Value {
    // Sanitize even callers constructing an in-memory or legacy snapshot.
    // Filter first so metadata from other models never enters the projection.
    let mut snapshot = snapshot.clone();
    snapshot.models.retain(|m| permitted.contains(&m.model));
    snapshot.invalid_models_dropped = 0;
    snapshot.invalid_fields_dropped = 0;
    duduclaw_odoo::schema::sanitize_snapshot(&mut snapshot);
    let query = query.trim().to_lowercase();
    let matches = |name: &str| query.is_empty() || name.to_lowercase().contains(&query);
    let models: Vec<Value> = snapshot.models.iter()
        .filter(|model| permitted.contains(&model.model))
        .filter(|model| model_filter.is_empty() || model.model == model_filter)
        .filter_map(|model| {
            let model_matches = matches(&model.model) || matches(&model.name);
            let fields: Vec<Value> = model.fields.iter()
                .filter(|field| model_matches || matches(&field.name) || matches(&field.label))
                .map(|field| {
                    let mut value = serde_json::to_value(field).unwrap_or(Value::Null);
                    if field.relation.as_ref().is_some_and(|r| !permitted.contains(r)) {
                        value["relation"] = Value::Null;
                    }
                    value
                }).collect();
            if !model_matches && fields.is_empty() { return None; }
            let mut value = serde_json::json!({
                "model": model.model, "name": model.name, "label_flagged": model.label_flagged, "custom": model.custom,
                "field_count": model.fields.len(), "truncated": model.truncated,
                "custom_fields": model.fields.iter().filter(|f| f.custom).map(|f| &f.name).collect::<Vec<_>>(),
            });
            if !model_filter.is_empty() || !query.is_empty() { value["fields"] = serde_json::json!(fields); }
            Some(value)
        }).collect();
    serde_json::json!({
        "untrusted_odoo_metadata": true,
        "note": duduclaw_odoo::schema::UNTRUSTED_METADATA_NOTE,
        "invalid_models_dropped": snapshot.invalid_models_dropped,
        "invalid_fields_dropped": snapshot.invalid_fields_dropped,
        "scanned_at": snapshot.scanned_at, "server_version": snapshot.server_version,
        "truncated": snapshot.truncated, "models": models})
}

/// Keep the same trust marker on every agent-facing metadata entry point.
pub(crate) fn schema_fields_metadata(report: &duduclaw_odoo::connector::SchemaFieldsReport) -> Value {
    serde_json::json!({
        "untrusted_odoo_metadata": true,
        "note": duduclaw_odoo::schema::UNTRUSTED_METADATA_NOTE,
        "invalid_models_dropped": 0,
        "invalid_fields_dropped": report.invalid_fields_dropped,
        "total_fields": report.total_fields,
        "truncated": report.truncated,
        "fields": report.fields,
    })
}
