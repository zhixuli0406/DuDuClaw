//! Account ACL metadata only. Never read records or infer denial from an error.
use std::collections::BTreeMap;
use std::time::Duration;
use serde::{Deserialize, Serialize};
use serde_json::json;
use crate::{check_blocklist, AccessKind, OdooConnector};

pub const ACCESS_CALL_TIMEOUT: Duration = Duration::from_millis(500);
pub const ACCESS_CONCURRENCY: usize = 8;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelAccess {
    pub read: Option<bool>,
    pub create: Option<bool>,
    pub write: Option<bool>,
    pub unlink: Option<bool>,
    pub checked_at: String,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum AccessMethod { Modern, ModernRecordset, Legacy }

impl OdooConnector {
    async fn access_call(&self, model: &str, method: AccessMethod, operation: &str) -> Option<bool> {
        let (name, args, kwargs) = match method {
            AccessMethod::Modern => ("has_access", vec![json!(operation)], json!({})),
            // Stock 18+ exposes has_access as a recordset method (no @api.model).
            // Empty ids check the model ACL without fetching business records.
            AccessMethod::ModernRecordset => ("has_access", vec![json!([]), json!(operation)], json!({})),
            AccessMethod::Legacy => ("check_access_rights", vec![json!(operation)], json!({"raise_exception": false})),
        };
        tokio::time::timeout(ACCESS_CALL_TIMEOUT, self.execute_kw(model, name, args, kwargs))
            .await.ok()?.ok()?.as_bool()
    }

    async fn account_access(&self, model: &str) -> ModelAccess {
        let mut rights = ModelAccess { checked_at: chrono::Utc::now().to_rfc3339(), ..ModelAccess::default() };
        // All four probes are Read metadata, regardless of the operation being
        // asked about. Still honour the gateway's fail-closed read block list.
        if !crate::schema::valid_model_name(model)
            || check_blocklist(model, AccessKind::Read, &self.access_unblocks).is_err() {
            return rights;
        }
        let (method, read_probed) = {
            let mut cached = self.access_method.lock().await;
            if let Some(method) = *cached { (method, false) } else {
                let version = self.status().version;
                let major = version.strip_prefix("saas~").unwrap_or(&version)
                    .split('.').next().and_then(|v| v.parse::<u32>().ok());
                let candidates = if major.is_some_and(|v| v < 18) {
                    [AccessMethod::Legacy, AccessMethod::Modern, AccessMethod::ModernRecordset]
                } else {
                    [AccessMethod::Modern, AccessMethod::ModernRecordset, AccessMethod::Legacy]
                };
                // Failure on one model is not proof of server-wide absence.
                // Keep trying the version default on subsequent models.
                let mut selected = if major.is_some_and(|v| v < 18) { AccessMethod::Legacy } else { AccessMethod::ModernRecordset };
                for candidate in candidates {
                    if let Some(read) = self.access_call(model, candidate, "read").await {
                        rights.read = Some(read);
                        selected = candidate;
                        break;
                    }
                }
                *cached = Some(selected);
                (selected, true)
            }
        };
        if !read_probed { rights.read = self.access_call(model, method, "read").await; }
        rights.create = self.access_call(model, method, "create").await;
        rights.write = self.access_call(model, method, "write").await;
        rights.unlink = self.access_call(model, method, "unlink").await;
        rights
    }

    /// Visit every scanned model, capped at 400. Eight workers issue at most
    /// eight requests at once. Worst-case call budget: 100s plus 1.5s detection.
    /// Errors/timeouts become null cells; page loads only read the snapshot.
    pub async fn probe_model_access(&self, models: Vec<String>) -> BTreeMap<String, ModelAccess> {
        self.probe_model_access_bounded(models, Duration::from_secs(120)).await.0
    }

    /// Return completed rows on deadline so the gateway can resume the missing
    /// models next cycle. Cancellation drops all borrowed workers together.
    pub async fn probe_model_access_bounded(
        &self, models: Vec<String>, budget: Duration,
    ) -> (BTreeMap<String, ModelAccess>, bool) {
        let models: Vec<_> = models.into_iter().take(crate::schema::MAX_MODELS).collect();
        let next = std::sync::atomic::AtomicUsize::new(0);
        let results = tokio::sync::Mutex::new(BTreeMap::new());
        let worker = || async {
            loop {
                let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(model) = models.get(index) else { break; };
                let access = self.account_access(model).await;
                results.lock().await.insert(model.clone(), access);
            }
        };
        // Borrowed futures avoid detached tasks when the scan is cancelled.
        let timed_out = tokio::time::timeout(budget, async {
            tokio::join!(worker(), worker(), worker(), worker(), worker(), worker(), worker(), worker());
        }).await.is_err();
        (results.into_inner(), timed_out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use crate::connector::mocked_connector;

    #[tokio::test]
    async fn modern_boolean_false_and_cell_errors_are_distinct() {
        let conn = mocked_connector(vec![Ok(json!(true)), Ok(json!(false)), Err("offline".into()), Ok(json!(true))]);
        let rights = conn.account_access("crm.lead").await;
        assert_eq!(rights.read, Some(true));
        assert_eq!(rights.create, Some(false));
        assert_eq!(rights.write, None);
        assert_eq!(rights.unlink, Some(true));
        assert!(!rights.checked_at.is_empty());
    }

    #[tokio::test]
    async fn modern_signature_then_legacy_fallback_is_cached_once() {
        let conn = mocked_connector(vec![Err("missing method".into()), Err("missing method".into()),
            Ok(json!(false)), Ok(json!(true)), Ok(json!(true)), Ok(json!(true)),
            Ok(json!(true)), Ok(json!(true)), Ok(json!(true)), Ok(json!(true))]);
        assert_eq!(conn.account_access("crm.lead").await.read, Some(false));
        assert_eq!(conn.account_access("sale.order").await.read, Some(true));
        assert!(matches!(*conn.access_method.lock().await, Some(AccessMethod::Legacy)));
    }

    #[tokio::test]
    async fn older_version_prefers_legacy_and_malformed_responses_are_unknown() {
        let conn = mocked_connector(vec![Ok(json!({"server_version":"17.0"})),
            Ok(json!(true)), Ok(json!("true")), Ok(Value::Null), Ok(json!(false))]);
        conn.version().await.unwrap();
        let rights = conn.account_access("crm.lead").await;
        assert!(matches!(*conn.access_method.lock().await, Some(AccessMethod::Legacy)));
        assert_eq!(rights.create, None);
        assert_eq!(rights.write, None);
        assert_eq!(rights.unlink, Some(false));
    }

    #[tokio::test]
    async fn unblocked_system_models_use_read_metadata_policy_for_every_operation() {
        let mut conn = mocked_connector(vec![Ok(json!(true)); 4]);
        conn.access_unblocks = vec!["ir.model".into()];
        // Policy still forbids mutation; asking about mutation is only metadata.
        assert!(check_blocklist("ir.model", AccessKind::Mutate, &conn.access_unblocks).is_err());
        assert_eq!(conn.account_access("ir.model").await.write, Some(true));
    }

    #[tokio::test]
    async fn probe_caps_models_at_snapshot_limit() {
        let conn = mocked_connector(vec![Ok(json!(true)); crate::schema::MAX_MODELS * 4]);
        let models = (0..crate::schema::MAX_MODELS + 20).map(|i| format!("x_model_{i}")).collect();
        assert_eq!(conn.probe_model_access(models).await.len(), crate::schema::MAX_MODELS);
    }

    #[tokio::test]
    async fn stock_recordset_signature_is_supported() {
        let conn = mocked_connector(vec![Err("wrong signature".into()), Ok(json!(true)),
            Ok(json!(true)), Ok(json!(false)), Ok(json!(false))]);
        assert_eq!(conn.account_access("crm.lead").await.write, Some(false));
        assert!(matches!(*conn.access_method.lock().await, Some(AccessMethod::ModernRecordset)));
    }

    #[tokio::test]
    async fn blocked_models_never_send_requests_and_errors_stay_unknown() {
        let conn = mocked_connector(vec![]);
        let rights = conn.account_access("res.users").await;
        assert_eq!(rights.read, None);
        assert!(conn.access_method.lock().await.is_none());
        let conn = mocked_connector(vec![Err("offline".into()); 6]);
        assert_eq!(conn.account_access("crm.lead").await.read, None);
    }

    #[tokio::test]
    async fn first_model_failure_does_not_poison_other_models() {
        let mut replies = vec![Err("offline".into()); 6];
        replies.extend(vec![Ok(json!(true)); 4]);
        let conn = mocked_connector(replies);
        assert_eq!(conn.account_access("crm.lead").await.read, None);
        assert!(matches!(*conn.access_method.lock().await, Some(AccessMethod::ModernRecordset)));
        assert_eq!(conn.account_access("sale.order").await.read, Some(true));
    }

    #[tokio::test]
    async fn explicit_rescan_resets_detection_and_rights() {
        let conn = mocked_connector(vec![Ok(json!(true)), Ok(json!(true)), Ok(json!(true)), Ok(json!(true)),
            Ok(json!(false)), Ok(json!(false)), Ok(json!(false)), Ok(json!(false))]);
        assert_eq!(conn.account_access("crm.lead").await.read, Some(true));
        conn.invalidate_compatible_fields().await;
        assert!(conn.access_method.lock().await.is_none());
        assert_eq!(conn.account_access("crm.lead").await.read, Some(false));
    }
}
