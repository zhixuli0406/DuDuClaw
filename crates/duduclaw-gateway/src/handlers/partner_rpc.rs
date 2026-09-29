//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Partner Portal ───────────────────────────────────────

    pub(crate) fn partner_store(&self) -> PartnerStore {
        PartnerStore::new(&self.home_dir.join("partner.db"))
    }

    pub(crate) async fn handle_partner_profile(&self) -> WsFrame {
        let store = self.partner_store();
        let profile = store.get_profile();
        match serde_json::to_value(&profile) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("serialize profile: {e}")),
        }
    }

    pub(crate) async fn handle_partner_stats(&self) -> WsFrame {
        let store = self.partner_store();
        let stats = store.compute_stats();
        match serde_json::to_value(&stats) {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("serialize stats: {e}")),
        }
    }

    pub(crate) async fn handle_partner_customers(&self, params: Value) -> WsFrame {
        let status = params
            .get("status")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(100)
            .min(1000) as usize;

        let store = self.partner_store();
        let customers = store.list_customers(status.as_deref(), limit);
        match serde_json::to_value(&customers) {
            Ok(list) => WsFrame::ok_response("", json!({ "customers": list })),
            Err(e) => WsFrame::error_response("", &format!("serialize customers: {e}")),
        }
    }

    pub(crate) async fn handle_partner_profile_update(&self, params: Value) -> WsFrame {
        let tier = match params.get("tier").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'tier' parameter"),
        };
        let input = PartnerProfileInput {
            company: params
                .get("company")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            tier,
            partner_id: params
                .get("partner_id")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
            certified_at: params
                .get("certified_at")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from),
        };
        let store = self.partner_store();
        match store.upsert_profile(&input) {
            Ok(()) => {
                let profile = store.get_profile();
                match serde_json::to_value(&profile) {
                    Ok(v) => WsFrame::ok_response("", v),
                    Err(e) => WsFrame::error_response("", &format!("serialize profile: {e}")),
                }
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_partner_customer_add(&self, params: Value) -> WsFrame {
        let input: PartnerCustomerInput = match serde_json::from_value(params.clone()) {
            Ok(v) => v,
            Err(e) => {
                return WsFrame::error_response("", &format!("Invalid customer payload: {e}"));
            }
        };
        let store = self.partner_store();
        match store.add_customer(&input) {
            Ok(id) => WsFrame::ok_response("", json!({ "id": id })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_partner_customer_update(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let patch_value = params.get("patch").cloned().unwrap_or_else(|| json!({}));
        let patch: PartnerCustomerPatch = match serde_json::from_value(patch_value) {
            Ok(v) => v,
            Err(e) => return WsFrame::error_response("", &format!("Invalid patch payload: {e}")),
        };
        let store = self.partner_store();
        match store.update_customer(&id, &patch) {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_partner_customer_delete(&self, params: Value) -> WsFrame {
        let id = match params.get("id").and_then(|v| v.as_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => return WsFrame::error_response("", "Missing 'id' parameter"),
        };
        let store = self.partner_store();
        match store.delete_customer(&id) {
            Ok(()) => WsFrame::ok_response("", json!({ "success": true })),
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}
