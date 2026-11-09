//! Dashboard bridge to the root-owned updater. Only version and correlation id cross.
use super::*;
use duduclaw_isolation::ids::RequestId;
use duduclaw_isolation::update::{UpdateRequest, UpdateRequestV1, UpdateResponse, UpdateResponseV1, UpdateStatus};

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApplyParams { request_id: Option<String> }

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusParams { request_id: String }

fn request_id(text: &str) -> Result<RequestId, &'static str> {
    let id = uuid::Uuid::parse_str(text).map_err(|_| "更新要求識別碼無效。")?;
    if id.hyphenated().to_string() != text || id.is_nil() {
        return Err("更新要求識別碼無效。");
    }
    Ok(RequestId(*id.as_bytes()))
}

async fn exchange(request: UpdateRequest) -> Result<UpdateResponse, &'static str> {
    #[cfg(target_os = "linux")]
    {
        tokio::task::spawn_blocking(move || duduclaw_updated::client::request(request))
            .await.map_err(|_| "更新服務目前無法連線，尚未確認更新結果。")?
            .map_err(|_| "更新服務目前無法連線，尚未確認更新結果。")
    }
    #[cfg(not(target_os = "linux"))]
    { let _ = request; Err("此平台不支援受保護更新。") }
}

fn response_frame(response: UpdateResponse, expected: RequestId, applying: bool) -> WsFrame {
    let (id, state) = match response {
        UpdateResponse::V1(UpdateResponseV1::Accepted { request_id }) if applying =>
            (request_id, UpdateStatus::Queued),
        UpdateResponse::V1(UpdateResponseV1::Status { request_id, state }) => (request_id, state),
        UpdateResponse::V1(UpdateResponseV1::Error { request_id, code }) if request_id == expected => {
            use duduclaw_isolation::update::UpdateErrorCode::*;
            let message = match code {
                Unauthorized => "更新服務拒絕此身分的要求。",
                InvalidRequest => "更新要求格式無效。",
                Busy => "另一項更新正在進行，請稍後再試。",
                UnknownRequest => "找不到這項更新要求，尚未確認更新結果。",
                SignatureRejected => "更新簽章驗證失敗，未安裝更新。",
                Expired => "更新驗證資料已過期，未安裝更新。",
                RollbackRefused => "更新版本或序號過舊，未安裝更新。",
                Incompatible => "此版本與目前安裝不相容，未安裝更新。",
                MaintenanceRequired => "更新服務需要人工修復，請聯絡管理員。",
            };
            if applying && code != UnknownRequest {
                return WsFrame::ok_response("", json!({
                    "success": false, "message": message, "needs_restart": false,
                    "protected_request_id": uuid::Uuid::from_bytes(expected.0).to_string(),
                    "protected_state": if code == MaintenanceRequired { UpdateStatus::Maintenance } else { UpdateStatus::Failed },
                }));
            }
            return WsFrame::error_response("", message);
        }
        _ => return WsFrame::error_response("", "更新服務回應不符，尚未確認更新結果。"),
    };
    if id != expected { return WsFrame::error_response("", "更新服務回應不符，尚未確認更新結果。"); }
    // Acceptance/preparation/activation are never reported as installation success.
    WsFrame::ok_response("", json!({
        "success": state == UpdateStatus::Committed,
        "message": "",
        "needs_restart": false,
        "protected_request_id": uuid::Uuid::from_bytes(id.0).to_string(),
        "protected_state": state,
    }))
}

impl MethodHandler {
    // Both entry points are Admin-gated by dispatch_system, including status.
    pub(crate) async fn handle_protected_update_apply(&self, params: Value) -> WsFrame {
        let params: ApplyParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(_) => return WsFrame::error_response("", "更新要求只能包含識別碼。"),
        };
        let id = match params.request_id.as_deref().map(request_id).transpose() {
            Ok(id) => id.unwrap_or_else(|| RequestId(*uuid::Uuid::new_v4().as_bytes())),
            Err(message) => return WsFrame::error_response("", message),
        };
        let pending = self.pending_update.read().await.clone();
        let Some(pending) = pending.filter(|pending| !pending.is_expired()) else {
            return WsFrame::error_response("", "請先重新檢查可用更新。");
        };
        let version = match duduclaw_updated::parse_version(&pending.version) {
            Ok(version) => version,
            Err(_) => return WsFrame::error_response("", "更新版本不是可接受的正式版本。"),
        };
        crate::security_autopilot::audit_and_emit(&self.home_dir,
            &duduclaw_security::audit::AuditEvent::new("system_update", "system",
                duduclaw_security::audit::Severity::Info,
                json!({ "action": "protected_request", "request_id": uuid::Uuid::from_bytes(id.0).to_string(),
                    "target_version": pending.version })));
        match exchange(UpdateRequest::V1(UpdateRequestV1::Apply { version, request_id: id })).await {
            Ok(response) => response_frame(response, id, true),
            Err(message) => WsFrame::error_response("", message),
        }
    }

    pub(crate) async fn handle_protected_update_status(&self, params: Value) -> WsFrame {
        if !crate::updater::is_native_protected_install() {
            return WsFrame::error_response("", "目前安裝不使用受保護更新服務。");
        }
        let params: StatusParams = match serde_json::from_value(params) {
            Ok(params) => params,
            Err(_) => return WsFrame::error_response("", "更新狀態要求格式無效。"),
        };
        let id = match request_id(&params.request_id) {
            Ok(id) => id,
            Err(message) => return WsFrame::error_response("", message),
        };
        match exchange(UpdateRequest::V1(UpdateRequestV1::Status { request_id: id })).await {
            Ok(response) => response_frame(response, id, false),
            Err(message) => WsFrame::error_response("", message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_update_params_reject_authority_fields() {
        for field in ["version", "url", "path", "argv", "root", "manifest"] {
            let mut value = json!({});
            value[field] = json!("untrusted");
            assert!(serde_json::from_value::<ApplyParams>(value.clone()).is_err());
            value["request_id"] = json!("00000000-0000-0000-0000-000000000001");
            assert!(serde_json::from_value::<StatusParams>(value).is_err());
        }
        assert!(request_id("00000000-0000-0000-0000-000000000001").is_ok());
        for invalid in ["", "../request", "00000000000000000000000000000001", "00000000-0000-0000-0000-000000000000"] {
            assert!(request_id(invalid).is_err());
        }
    }

    #[test]
    fn protected_update_unknown_request_never_claims_failure_or_success() {
        let id = RequestId([1; 16]);
        let response = UpdateResponse::V1(UpdateResponseV1::Error {
            request_id: id, code: duduclaw_isolation::update::UpdateErrorCode::UnknownRequest,
        });
        for applying in [false, true] {
            let value = serde_json::to_value(response_frame(response.clone(), id, applying)).unwrap();
            assert_eq!(value["ok"], json!(false));
            assert!(value.get("payload").is_none());
        }
    }

    #[test]
    fn protected_update_acceptance_is_not_committed() {
        let id = RequestId([1; 16]);
        for state in [UpdateStatus::Queued, UpdateStatus::Downloading, UpdateStatus::Prepared,
            UpdateStatus::Activated, UpdateStatus::Failed, UpdateStatus::Maintenance, UpdateStatus::Committed] {
            let frame = response_frame(UpdateResponse::V1(UpdateResponseV1::Status { request_id: id, state }), id, false);
            let json = serde_json::to_value(frame).unwrap();
            assert_eq!(json["payload"]["success"], json!(state == UpdateStatus::Committed));
        }
        let wrong = response_frame(UpdateResponse::V1(UpdateResponseV1::Accepted { request_id: RequestId([2; 16]) }), id, true);
        assert!(!serde_json::to_string(&wrong).unwrap().contains("protected_request_id"));
    }
}
