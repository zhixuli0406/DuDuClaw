//! `manager_user_id` on `users.create` / `users.update` and the coded
//! errors the reporting-line rules return (`duduclaw_auth::org`).

use duduclaw_auth::OrgError;
use serde_json::{Value, json};

use crate::protocol::WsFrame;

/// Longest manager id accepted from a client (user ids are UUIDs).
const MAX_MANAGER_ID_LEN: usize = 128;

/// What a request asked for `manager_user_id`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ManagerChange {
    /// Key absent: leave it as it is.
    Unchanged,
    /// `null` or `""`: remove the manager.
    Clear,
    Set(String),
}

impl ManagerChange {
    pub(crate) fn target(&self) -> Option<&str> {
        match self {
            Self::Set(id) => Some(id),
            _ => None,
        }
    }
}

/// Parse `params.manager_user_id`. Anything other than a string or `null`
/// is refused rather than ignored.
pub(crate) fn parse_manager_change(params: &Value) -> Result<ManagerChange, WsFrame> {
    match params.get("manager_user_id") {
        None => Ok(ManagerChange::Unchanged),
        Some(Value::Null) => Ok(ManagerChange::Clear),
        Some(Value::String(s)) => {
            let id = s.trim();
            if id.is_empty() {
                Ok(ManagerChange::Clear)
            } else if id.len() > MAX_MANAGER_ID_LEN || id.chars().any(char::is_control) {
                Err(WsFrame::error_response("", "invalid manager_user_id"))
            } else {
                Ok(ManagerChange::Set(id.to_string()))
            }
        }
        Some(_) => Err(WsFrame::error_response("", "manager_user_id must be a string or null")),
    }
}

/// Plain-language zh-TW copy for each code (the dashboard shows `message`
/// and may branch on `code`).
fn org_error_message(e: &OrgError) -> &'static str {
    match e {
        OrgError::UserNotFound => "找不到這位使用者。",
        OrgError::SelfManager => "不能把自己設成自己的主管。",
        OrgError::Cycle => "這樣設定會讓主管關係繞成一圈，請改選其他主管。",
        OrgError::TooDeep => "組織層級最多 8 層，這樣設定會超過上限。",
        OrgError::ManagerRoleTooLow => "直屬主管的角色必須是主管或管理員。",
        OrgError::ManagerNotFound => "找不到指定的主管。",
        OrgError::ManagerInactive => "指定的主管目前已停權或離職。",
        OrgError::HasReports => "這位使用者還有直屬部屬，請先把部屬改掛到其他主管，再調整角色。",
        OrgError::TransferTargetInvalid => "交接對象必須是另一位在職的使用者。",
        OrgError::Db(_) => "目前無法更新組織關係，請稍後再試。",
    }
}

/// Coded error frame (`{code, message}`), the same shape as the edition and
/// forced-password-change gates. Database detail goes to the log, not the
/// client.
pub(crate) fn org_error_frame(e: &OrgError) -> WsFrame {
    if let OrgError::Db(detail) = e {
        tracing::warn!(error = %detail, "reporting-line write failed");
    }
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({
            "code": e.code(),
            "message": org_error_message(e),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_null_empty_and_value_are_distinguished() {
        assert_eq!(parse_manager_change(&json!({})).unwrap(), ManagerChange::Unchanged);
        assert_eq!(
            parse_manager_change(&json!({"manager_user_id": null})).unwrap(),
            ManagerChange::Clear
        );
        assert_eq!(
            parse_manager_change(&json!({"manager_user_id": "  "})).unwrap(),
            ManagerChange::Clear
        );
        assert_eq!(
            parse_manager_change(&json!({"manager_user_id": " m1 "})).unwrap(),
            ManagerChange::Set("m1".into())
        );
        assert!(parse_manager_change(&json!({"manager_user_id": 5})).is_err());
        assert!(parse_manager_change(&json!({"manager_user_id": "x".repeat(200)})).is_err());
    }

    #[test]
    fn error_frames_carry_the_stable_codes() {
        for (e, code) in [
            (OrgError::Cycle, "manager_cycle"),
            (OrgError::SelfManager, "manager_cycle"),
            (OrgError::TooDeep, "manager_too_deep"),
            (OrgError::ManagerRoleTooLow, "manager_role"),
            (OrgError::HasReports, "manager_has_reports"),
        ] {
            match org_error_frame(&e) {
                WsFrame::Response { ok: false, error: Some(err), .. } => {
                    assert_eq!(err["code"], code);
                    assert!(!err["message"].as_str().unwrap().contains("manager_"));
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        match org_error_frame(&OrgError::Db("disk I/O error at /secret/path".into())) {
            WsFrame::Response { error: Some(err), .. } => {
                assert!(!err.to_string().contains("/secret/path"), "no DB detail to the client");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}
