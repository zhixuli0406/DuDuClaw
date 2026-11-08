use super::*;

fn blocked_odoo_method(method: &str) -> bool {
    ["unlink", "uninstall", "uninstall_hook", "init", "_auto_init", "_register_hook", "signal_workflow", "execute_import"].contains(&method)
}

fn odoo_numeric<T: std::str::FromStr>(params: &Value, name: &str) -> std::result::Result<T, String> {
    let value = params.get(name).ok_or_else(|| format!("{name} is required"))?;
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        _ => return Err(format!("{name} must be a number or numeric string")),
    };
    text.parse().map_err(|_| format!("{name} must be a valid number or numeric string"))
}

pub(super) fn odoo_positive_integer(params: &Value, name: &str) -> std::result::Result<i64, String> {
    let value = odoo_numeric::<i64>(params, name)
        .map_err(|error| format!("{error}; expected a positive integer (1..={})", i64::MAX))?;
    if value <= 0 {
        return Err(format!("{name} must be a positive integer"));
    }
    Ok(value)
}

pub(super) fn odoo_limit(params: &Value, default: usize, cap: usize) -> std::result::Result<usize, String> {
    if params.get("limit").is_none() { return Ok(default); }
    let value = odoo_positive_integer(params, "limit")?;
    Ok(value.min(cap as i64) as usize)
}

pub(super) fn odoo_decimal(params: &Value, name: &str, default: f64) -> std::result::Result<f64, String> {
    if params.get(name).is_none() { return Ok(default); }
    let value = odoo_numeric::<f64>(params, name)?;
    if !value.is_finite() || value <= 0.0 {
        return Err(format!("{name} must be a finite positive number"));
    }
    Ok(value)
}

pub(crate) async fn handle_odoo_tool(
    tool: &str,
    params: &Value,
    home_dir: &Path,
    odoo: &OdooState,
    caller_agent: &str,
) -> Value {
    handle_odoo_tool_with_connector(tool, params, home_dir, odoo, caller_agent, None).await
}

/// Isolation supplies the already selected connector; no account resolution or
/// pool lookup is allowed on this path. Business gates below remain shared.
pub(crate) async fn handle_bound_odoo_tool(
    tool: &str, params: &Value, home_dir: &Path, odoo: &OdooState, caller_agent: &str,
    connector: std::sync::Arc<duduclaw_odoo::OdooConnector>,
) -> Value {
    handle_odoo_tool_with_connector(tool, params, home_dir, odoo, caller_agent, Some(connector)).await
}

async fn handle_odoo_tool_with_connector(
    tool: &str, params: &Value, home_dir: &Path, odoo: &OdooState, caller_agent: &str,
    bound: Option<std::sync::Arc<duduclaw_odoo::OdooConnector>>,
) -> Value {
    use duduclaw_odoo::connector::OdooConnector;
    use duduclaw_odoo::models::{accounting, crm, inventory, sale};

    if let Some(connector) = &bound {
        if connector.verify_io().is_err() { return mcp_error("Odoo authority revoked"); }
        if matches!(tool, "odoo_connect" | "odoo_status") {
            let key = odoo.pool_key(caller_agent).await;
            if connector.verify_io().is_err() { return mcp_error("Odoo authority revoked"); }
            return tool_text(&odoo_connect_text(&connector.status(), &key.0, &key.1));
        }
    }


    // odoo_connect doesn't require an existing connection
    if tool == "odoo_connect" {
        return handle_odoo_connect(home_dir, odoo, caller_agent).await;
    }

    if tool == "odoo_status" {
        return match odoo.is_connected(caller_agent).await {
            true => {
                match odoo.cached_connector(caller_agent).await {
                    Ok(conn) => {
                        let s = conn.status();
                        let key = odoo.pool_key(caller_agent).await;
                        let visibility = odoo.schema_visibility(caller_agent).await;
                        let schema_line = schema_status_line(home_dir, &conn, &key.1, &s.version, &visibility);
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

    // RM-120: generic create / write carry their own explicit-grant gate
    // chain (`odoo_generic_write.rs`); the permissive per-agent filter below
    // never authorizes them.
    if matches!(tool, "odoo_create" | "odoo_write") {
        // The generic-write gate chain resolves its connection through the
        // pool, which the isolation-bound path must never reach. Fail closed.
        if bound.is_some() {
            return super::egress_gate::pre_dispatch_error("This Odoo capability is not available. This is a policy decision; retrying the same call will not succeed. 這項 Odoo 功能不提供；這是政策限制，重試相同呼叫不會成功。");
        }
        return super::odoo_generic_write::handle_odoo_generic_write(tool, params, home_dir, odoo, caller_agent).await;
    }

    if !matches!(tool,
        "odoo_schema_overview" | "odoo_schema_fields" | "odoo_crm_leads" | "odoo_crm_create_lead"
        | "odoo_crm_update_stage" | "odoo_sale_orders" | "odoo_sale_create_quotation" | "odoo_sale_confirm"
        | "odoo_inventory_products" | "odoo_inventory_check" | "odoo_invoice_list" | "odoo_payment_status"
        | "odoo_search" | "odoo_execute" | "odoo_report" | "odoo_partner_search") {
        return super::egress_gate::pre_dispatch_error("This Odoo capability is not available. This is a policy decision; retrying the same call will not succeed. 這項 Odoo 功能不提供；這是政策限制，重試相同呼叫不會成功。");
    }

    // Local-only preparation. Preserve a registered override when no agent file exists.
    let config_result = odoo.load_global_for_gate(home_dir).await;
    let agent_path = caller_agent_dir(home_dir, caller_agent).join("agent.toml");
    match tokio::fs::read_to_string(&agent_path).await {
        Ok(raw) => match duduclaw_odoo::AgentOdooConfig::parse_agent_toml(&raw) {
            Ok(Some(cfg)) => odoo.register_agent(caller_agent, cfg).await,
            Ok(None) => odoo.unregister_agent(caller_agent).await,
            Err(error) => return mcp_error(&error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
        Err(_) => return mcp_error(duduclaw_odoo::ODOO_UNREADABLE),
    }

    // RFC-21 §2 acceptance: defence-in-depth. Reject the call before any
    // HTTP round-trip leaves the process when `agent.toml [odoo]
    // .allowed_models` / `.allowed_actions` doesn't cover it.
    if let Some((verb, model)) = classify_odoo_call(tool, params) {
        // v1.68.0: the dashboard's 功能模組 switches (`[odoo] features_*`).
        if config_result.is_ok() && let Err(module) = odoo.feature_gate(&model).await {
            duduclaw_security::audit::append_tool_call(
                home_dir,
                caller_agent,
                tool,
                &format!("DENIED: {model}/{verb} — Odoo module '{module}' is switched off"),
                false,
            );
            return super::egress_gate::pre_dispatch_error(&format!(
                "Odoo feature_gate: module '{module}' is switched off in the dashboard (Odoo → 功能模組); \
                 '{model}' is not available. This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。"
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
            return super::egress_gate::pre_dispatch_error(&format!("Odoo permission denied: {reason}. This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。"));
        }
    }

    if let Err(error) = config_result {
        return super::egress_gate::pre_dispatch_error(&error);
    }
    let unblock_models = odoo.unblock_models(caller_agent).await;
    if matches!(tool, "odoo_search" | "odoo_execute") {
        let model = params.get("model").and_then(Value::as_str).unwrap_or("");
        let method = params.get("method").and_then(Value::as_str).unwrap_or("");
        if model.is_empty() || (tool == "odoo_execute" && method.is_empty()) {
            return mcp_error("model and method are required for execute; model is required for search");
        }
        let kind = if tool == "odoo_search" || odoo_method_to_verb(method) == "read" {
            duduclaw_odoo::AccessKind::Read
        } else { duduclaw_odoo::AccessKind::Mutate };
        if let Err(denial) = duduclaw_odoo::check_blocklist(model, kind, &unblock_models) {
            return super::egress_gate::pre_dispatch_error(&blocklist_denial_message(model, denial));
        }
        if tool == "odoo_execute" && blocked_odoo_method(method) {
            return super::egress_gate::pre_dispatch_error(&method_denial_message(method));
        }
    }
    // Daybreak (a): schema metadata of a blocked model (its fields, labels,
    // relations) is never returned, live or from the snapshot.
    if matches!(tool, "odoo_schema_overview" | "odoo_schema_fields") {
        let model = params.get("model").and_then(Value::as_str).unwrap_or("");
        if !model.is_empty()
            && let Err(denial) = duduclaw_odoo::check_blocklist(model, duduclaw_odoo::AccessKind::Read, &unblock_models)
        {
            return super::egress_gate::pre_dispatch_error(&blocklist_denial_message(model, denial));
        }
    }
    if tool == "odoo_inventory_check" {
        if let Err(module) = odoo.feature_gate("product.product").await {
            return super::egress_gate::pre_dispatch_error(&format!("Odoo feature_gate: module '{module}' is switched off. This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。"));
        }
        let cfg = odoo.agent_override(caller_agent).await;
        if let Err(reason) = crate::odoo_pool::check_action_permission(cfg.as_ref(), "search", "product.product") {
            return super::egress_gate::pre_dispatch_error(&format!("Odoo permission denied: {reason}. This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。"));
        }
    }
    // Isolation: the already selected connector is used as-is; no account
    // resolution, auto-connect or pool lookup happens on that path.
    let conn_arc = if let Some(connector) = bound { connector } else {
        // Missing endpoint / credential source is a local failure: refuse before
        // auto-connect or secret resolution so it attests "not dispatched".
        if let Err(error) = odoo.connect_precheck(caller_agent).await {
            return super::egress_gate::pre_dispatch_error(&format!("Odoo not connected for agent '{caller_agent}'. Call odoo_connect first. {error}"));
        }
        match connect_odoo_pool(home_dir, odoo, caller_agent).await {
            Ok(conn) => conn,
            Err(error) => return mcp_error(&format!("Odoo not connected for agent '{caller_agent}'. Call odoo_connect first. {error}")),
        }
    };
    if conn_arc.verify_io().is_err() { return mcp_error("Odoo authority revoked"); }
    let conn: &OdooConnector = conn_arc.as_ref();
    // ── per-call audit attribution (RFC-21 §2 acceptance) ────────────────
    // Surface caller_agent + profile + tool + params summary to
    // tool_calls.jsonl so the audit trail attributes Odoo activity to a
    // specific agent rather than to the global admin user.
    let _audit_profile = odoo.pool_key(caller_agent).await.1;
    // A: effective block-list opt-out for this agent (per-agent override, else
    // global). Empty ⇒ the built-in security block list is fully in force.
    let result: std::result::Result<String, String> = match tool {
        "odoo_schema_overview" => {
            let profile = conn.schema_profile(&_audit_profile);
            if conn.verify_io().is_err() { return mcp_error("Odoo authority revoked"); }
            match duduclaw_odoo::schema::load_snapshot(home_dir, &profile) {
                Ok(Some(snapshot)) if snapshot.connection_identity == conn.connection_identity() => {
                    // Feature switches, allowed_models / actions and the block
                    // list with this caller's effective unblocks (Daybreak (a)).
                    let visibility = odoo.schema_visibility(caller_agent).await;
                    let permitted = permitted_schema_models(&snapshot, &visibility);
                    let model = params.get("model").and_then(Value::as_str).unwrap_or("");
                    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
                    Ok(schema_overview(&snapshot, &permitted, model, query).to_string())
                }
                Ok(Some(_)) => Err("Odoo schema snapshot belongs to a different connection; 結構正在背景準備中；可先用 odoo_schema_fields 查單一資料表 / Schema is being prepared in the background; use odoo_schema_fields for a specific model meanwhile".into()),
                Ok(None) => Err("Odoo 結構正在背景準備中；可先用 odoo_schema_fields 查詢單一資料表。 / Schema is being prepared in the background; use odoo_schema_fields for a specific model meanwhile.".into()),
                Err(error) => Err(error),
            }
        }
        "odoo_crm_leads" => {
            let stage = params.get("stage").and_then(|v| v.as_str()).unwrap_or("");
            let limit = match odoo_limit(params, 20, 100) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            if params.get("expected_revenue").is_some() {
                let value = match odoo_decimal(params, "expected_revenue", 1.0) {
                    Ok(value) => value,
                Err(error) => return mcp_error(&error),
                };
                vals["expected_revenue"] = serde_json::json!(value);
            }
            match conn.create("crm.lead", vals).await {
                Ok(id) => Ok(format!("CRM lead created (ID: {id})")),
                Err(e) => Err(e),
            }
        }
        "odoo_crm_update_stage" => {
            let lead_id = match odoo_positive_integer(params, "lead_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
            let stage_name = params
                .get("stage_name")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if stage_name.is_empty() {
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
            let limit = match odoo_limit(params, 20, 100) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            let partner_id = match odoo_positive_integer(params, "partner_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
            let product_id = match odoo_positive_integer(params, "product_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
            let qty = match odoo_decimal(params, "quantity", 1.0) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            let order_id = match odoo_positive_integer(params, "order_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            let limit = match odoo_limit(params, 20, 100) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
            let storable_only = match params.get("storable_only") {
                None => false,
                Some(Value::Bool(value)) => *value,
                Some(_) => return mcp_error("storable_only must be a boolean"),
            };
            async {
                let domain = conn.inventory_product_domain(query, storable_only).await?;
                let data = conn.search_read(
                    "product.product", domain, inventory::PRODUCT_FIELDS, limit,
                ).await?;
                let products: Vec<inventory::Product> = data
                    .as_array()
                    .unwrap_or(&vec![])
                    .iter()
                    .map(inventory::map_product)
                    .collect();
                Ok(serde_json::to_string_pretty(&products).unwrap_or_default())
            }.await
        }
        "odoo_inventory_check" => {
            let product_id = match odoo_positive_integer(params, "product_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
            // This tool also reads product totals; preserve both model gates.
            if let Err(module) = odoo.feature_gate("product.product").await {
                return mcp_error(&format!("Odoo feature_gate: module '{module}' is switched off"));
            }
            let cfg = odoo.agent_override(caller_agent).await;
            if let Err(reason) = crate::odoo_pool::check_action_permission(cfg.as_ref(), "search", "product.product") {
                return mcp_error(&format!("Odoo permission denied: {reason}"));
            }
            conn.inventory_check(product_id).await
                .map(|data| serde_json::to_string_pretty(&data).unwrap_or_default())
        }
        "odoo_invoice_list" => {
            let status = params.get("status").and_then(|v| v.as_str()).unwrap_or("");
            let limit = match odoo_limit(params, 20, 100) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            let invoice_id = match odoo_positive_integer(params, "invoice_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            let limit = match odoo_limit(params, 20, 100) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
            let record_id = match odoo_positive_integer(params, "record_id") {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
            if report_name.is_empty() {
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
            let limit = match odoo_limit(params, 10, 40) {
                Ok(value) => value,
                Err(error) => return mcp_error(&error),
            };
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
                Ok(mut fields) => {
                    // Never name a model this caller may not see via a relation.
                    let visibility = odoo.schema_visibility(caller_agent).await;
                    hide_invisible_relations(&mut fields, &visibility);
                    Ok(schema_fields_metadata(&fields).to_string())
                }
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
             it to the employee's allowed tables and confirming sensitive access (dashboard: 員工 → 編輯 → 整合 → Odoo). \
             This is a policy decision; retrying the same call will not succeed. 這是政策限制，重試相同呼叫不會成功。"
        ),
        duduclaw_odoo::BlockDenial::SystemReadOnly => format!(
            "Odoo block list gate (system read-only): model '{model}' may be read with unblock_models in the agent's Odoo settings; writing or deleting is always refused, even when unblocked. This capability is not available. This is a policy decision; retrying the same call will not succeed. 這項功能不提供；這是政策限制，重試相同呼叫不會成功。"
        ),
    }
}

pub(crate) fn method_denial_message(method: &str) -> String {
    format!(
        "Odoo method safety gate: method '{method}' is blocked. This fixed safety rule cannot be changed by allowed_actions or unblock_models in the agent's Odoo settings. This capability is not available. This is a policy decision; retrying the same call will not succeed. 這項功能不提供；這是政策限制，重試相同呼叫不會成功。"
    )
}

/// Map an Odoo ORM `method` name to the coarse verb used by the per-agent
/// `allowed_actions` filter. CRUD methods map to `read`/`create`/`write`/
/// `unlink`; `action_*` / `button_*` / `toggle_*` workflow methods map to
/// `action` (or a model-qualified `action:model` permission);
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
        // `action` family so they don't slip through as `execute`.
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
    let global_cfg: duduclaw_odoo::OdooConfig = match global_table.get("odoo").cloned().map(|value| value.try_into::<duduclaw_odoo::OdooConfig>()) {
        Some(Ok(config)) => config,
        _ => return mcp_error("Odoo not connected: cannot read Odoo configuration. Call odoo_connect after checking the saved settings."),
    };
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
    let _ = odoo.load_global_for_gate(home_dir).await;
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

    let connector = match connect_odoo_pool(home_dir, odoo, caller_agent).await {
        Ok(conn) => conn,
        Err(error) => return mcp_error(&format!("Odoo connection failed: {error}")),
    };

    let status = connector.status();
    let key = odoo.pool_key(caller_agent).await;
    serde_json::json!({
        "content": [{"type": "text", "text": odoo_connect_text(&status, &key.0, &key.1)}]
    })
}

pub(crate) async fn connect_odoo_pool(
    home_dir: &Path, odoo: &OdooState, caller_agent: &str,
) -> std::result::Result<std::sync::Arc<duduclaw_odoo::OdooConnector>, String> {
    let home_dir_owned = home_dir.to_path_buf();
    let connector = odoo
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
        .await?;
    // RM-119 / Daybreak (a): schema scans are owned by the gateway, which
    // scans each effective identity under its own policy. An MCP turn —
    // auto-connect or odoo_connect — never starts one.
    Ok(connector)
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
    visibility: &crate::odoo_pool::SchemaVisibility,
) -> String {
    let profile = conn.schema_profile(profile);
    let mut line = match duduclaw_odoo::schema::load_snapshot(home, &profile) {
        Ok(Some(mut snapshot)) if snapshot.connection_identity == conn.connection_identity() => {
            // Even counts are derived only from models this caller may see.
            snapshot.models.retain(|m| visibility.allows(&m.model));
            snapshot.status_line(live_version)
        }
        Ok(None) => "Odoo 結構正在背景準備中；可先用 odoo_schema_fields 查詢單一資料表。 / Schema is being prepared in the background; use odoo_schema_fields for a specific model meanwhile.".into(),
        _ => "Schema snapshot: unavailable; re-scan recommended".into(),
    };
    if duduclaw_odoo::schema::load_scan_status(home, &profile).last_error.is_some() {
        line.push_str("; last schema scan failed; check Odoo connectivity and metadata permissions, then re-scan");
    }
    line
}

/// Snapshot models the caller may see (Daybreak (a)): feature switches,
/// allowed_models / allowed_actions, and the block list with the caller's
/// effective unblocks. A snapshot written before the gateway filtered scans
/// may still hold blocked models; they are dropped here.
pub(crate) fn permitted_schema_models(
    snapshot: &duduclaw_odoo::schema::SchemaSnapshot,
    visibility: &crate::odoo_pool::SchemaVisibility,
) -> std::collections::HashSet<String> {
    snapshot.models.iter()
        .filter(|m| visibility.allows(&m.model))
        .map(|m| m.model.clone())
        .collect()
}

/// Strip relations that would name a model the caller may not see.
pub(crate) fn hide_invisible_relations(
    report: &mut duduclaw_odoo::connector::SchemaFieldsReport,
    visibility: &crate::odoo_pool::SchemaVisibility,
) {
    for field in &mut report.fields {
        if field.relation.as_deref().is_some_and(|r| !visibility.allows(r)) {
            field.relation = None;
        }
    }
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
    let selected: Vec<_> = snapshot.models.iter()
        .filter(|m| model_filter.is_empty() || m.model == model_filter)
        .filter(|m| matches(&m.model) || matches(&m.name)
            || (!query.is_empty() && m.fields.iter().any(|f| matches(&f.name) || matches(&f.label))))
        .collect();
    // Broad searches remain an index. Field detail requires one model or a
    // query matching at most five authorized models.
    let include_fields = !model_filter.is_empty() || (!query.is_empty() && selected.len() <= 5);
    let cap = if include_fields { 32 * 1024 } else { 16 * 1024 };
    let total_count = selected.len();
    let models: Vec<Value> = selected.into_iter().map(|model| {
        let mut value = serde_json::json!({
            "model":model.model,
            "name":duduclaw_core::truncate_bytes(&model.name, 120),
        });
        if include_fields {
            value["label_flagged"] = serde_json::json!(model.label_flagged);
            let fields: Vec<_> = model.fields.iter()
                .filter(|f| matches(&model.model) || matches(&model.name) || matches(&f.name) || matches(&f.label))
                .map(|f| {
                    let mut field = serde_json::to_value(f).unwrap_or(Value::Null);
                    if f.relation.as_ref().is_some_and(|r| !permitted.contains(r)) {
                        field["relation"] = Value::Null;
                    }
                    field
                }).collect();
            value["fields"] = serde_json::json!(fields);
            value["field_count"] = serde_json::json!(model.total_fields);
            value["truncated"] = serde_json::json!(model.truncated);
        }
        value
    }).collect();
    let mut output = serde_json::json!({
        "untrusted_odoo_metadata":true,
        "note":duduclaw_odoo::schema::UNTRUSTED_METADATA_NOTE,
        "invalid_models_dropped":snapshot.invalid_models_dropped,
        "invalid_fields_dropped":snapshot.invalid_fields_dropped,
        "scanned_at":snapshot.scanned_at,"server_version":snapshot.server_version,
        "total_count":total_count,"truncated":snapshot.truncated,
        "fields_included":include_fields,
        "truncated_note":"If truncated, narrow the query or use odoo_schema_fields for a specific model.",
        "models":models,
    });
    // Keep JSON valid and within the serialized byte cap, including escaping.
    while serde_json::to_vec(&output).map_or(true, |v| v.len() > cap) {
        output["truncated"] = serde_json::json!(true);
        let Some(models) = output["models"].as_array_mut() else { break; };
        let Some(last) = models.last_mut() else { break; };
        if let Some(fields) = last.get_mut("fields").and_then(Value::as_array_mut)
            && !fields.is_empty() {
            fields.pop();
            last["truncated"] = serde_json::json!(true);
        } else { models.pop(); }
    }
    output
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
