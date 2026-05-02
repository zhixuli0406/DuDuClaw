//! Governance API 錯誤碼定義
//!
//! 定義 governance 端點回傳的 HTTP 狀態碼與錯誤代碼字串，
//! 提供從 `EvaluationResult` 到 `PolicyApiError` 的對應邏輯。
//!
//! ## 錯誤碼對照表
//!
//! | 錯誤碼                   | HTTP 狀態 | 說明                         |
//! |--------------------------|-----------|------------------------------|
//! | POLICY_RATE_EXCEEDED     | 403       | 速率限制超限                 |
//! | POLICY_PERMISSION_DENIED | 403       | 權限不足                     |
//! | POLICY_QUOTA_EXCEEDED    | 403       | 配額耗盡                     |
//! | POLICY_NOT_FOUND         | 404       | 政策不存在                   |
//! | POLICY_CONFLICT          | 409       | 政策衝突                     |
//! | POLICY_INVALID_SCHEMA    | 422       | 政策 schema 驗證失敗         |
//! | POLICY_APPROVAL_REQUIRED | 202       | 操作需等待核准（非錯誤）     |

use serde::{Deserialize, Serialize};

use crate::evaluator::{EvaluationResult, ViolationType};

// ── PolicyErrorCode ───────────────────────────────────────────────────────────

/// Governance API 錯誤碼枚舉。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PolicyErrorCode {
    /// 速率限制超限（HTTP 403）。
    RateExceeded,
    /// 權限不足（HTTP 403）。
    PermissionDenied,
    /// 配額耗盡（HTTP 403）。
    QuotaExceeded,
    /// 政策不存在（HTTP 404）。
    NotFound,
    /// 政策衝突（HTTP 409）。
    Conflict,
    /// 政策 schema 驗證失敗（HTTP 422）。
    InvalidSchema,
    /// 需等待核准（HTTP 202，非錯誤）。
    ApprovalRequired,
    /// Lifecycle 政策違規（HTTP 403）。
    LifecycleViolation,
}

impl PolicyErrorCode {
    /// 對應的 HTTP 狀態碼。
    pub fn http_status(&self) -> u16 {
        match self {
            Self::RateExceeded => 403,
            Self::PermissionDenied => 403,
            Self::QuotaExceeded => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::InvalidSchema => 422,
            Self::ApprovalRequired => 202,
            Self::LifecycleViolation => 403,
        }
    }

    /// 錯誤碼字串（供 API response body 使用）。
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::RateExceeded => "POLICY_RATE_EXCEEDED",
            Self::PermissionDenied => "POLICY_PERMISSION_DENIED",
            Self::QuotaExceeded => "POLICY_QUOTA_EXCEEDED",
            Self::NotFound => "POLICY_NOT_FOUND",
            Self::Conflict => "POLICY_CONFLICT",
            Self::InvalidSchema => "POLICY_INVALID_SCHEMA",
            Self::ApprovalRequired => "POLICY_APPROVAL_REQUIRED",
            Self::LifecycleViolation => "POLICY_LIFECYCLE_VIOLATION",
        }
    }
}

impl std::fmt::Display for PolicyErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.error_code())
    }
}

impl From<&ViolationType> for PolicyErrorCode {
    fn from(vt: &ViolationType) -> Self {
        match vt {
            ViolationType::RateExceeded => Self::RateExceeded,
            ViolationType::PermissionDenied => Self::PermissionDenied,
            ViolationType::QuotaExceeded => Self::QuotaExceeded,
            ViolationType::ApprovalRequired => Self::ApprovalRequired,
            ViolationType::LifecycleViolation => Self::LifecycleViolation,
        }
    }
}

// ── PolicyApiError ────────────────────────────────────────────────────────────

/// API 層錯誤結構，包含錯誤碼、HTTP 狀態和詳細訊息。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyApiError {
    error_code: PolicyErrorCode,
    pub policy_id: Option<String>,
    pub message: String,
}

impl PolicyApiError {
    /// 建立 PolicyApiError。
    pub fn new(
        error_code: PolicyErrorCode,
        policy_id: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            error_code,
            policy_id,
            message: message.into(),
        }
    }

    /// 從 EvaluationResult 建立 PolicyApiError。
    ///
    /// 若 result.allowed == true 且無 violation_type，回傳 `None`。
    pub fn from_evaluation_result(result: &EvaluationResult) -> Option<Self> {
        let violation_type = result.violation_type.as_ref()?;

        // allowed=true 且只有 warn（RateExceeded with allowed）→ 不視為 API 錯誤
        if result.allowed && matches!(violation_type, ViolationType::RateExceeded) {
            return None;
        }
        // allowed=true，無其他違規 → None
        if result.allowed && result.violation_type.is_none() {
            return None;
        }

        let error_code = PolicyErrorCode::from(violation_type);
        Some(Self {
            error_code,
            policy_id: result.policy_id.clone(),
            message: result.message.clone(),
        })
    }

    /// 取得 HTTP 狀態碼。
    pub fn http_status(&self) -> u16 {
        self.error_code.http_status()
    }

    /// 取得錯誤碼。
    pub fn error_code(&self) -> &PolicyErrorCode {
        &self.error_code
    }
}

impl std::fmt::Display for PolicyApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} (HTTP {}): {}",
            self.error_code.error_code(),
            self.error_code.http_status(),
            self.message
        )
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::evaluator::ViolationType;

    #[test]
    fn test_all_error_codes_have_correct_http_status() {
        assert_eq!(PolicyErrorCode::RateExceeded.http_status(), 403);
        assert_eq!(PolicyErrorCode::PermissionDenied.http_status(), 403);
        assert_eq!(PolicyErrorCode::QuotaExceeded.http_status(), 403);
        assert_eq!(PolicyErrorCode::NotFound.http_status(), 404);
        assert_eq!(PolicyErrorCode::Conflict.http_status(), 409);
        assert_eq!(PolicyErrorCode::InvalidSchema.http_status(), 422);
        assert_eq!(PolicyErrorCode::ApprovalRequired.http_status(), 202);
        assert_eq!(PolicyErrorCode::LifecycleViolation.http_status(), 403);
    }

    #[test]
    fn test_violation_type_to_error_code_conversion() {
        assert_eq!(
            PolicyErrorCode::from(&ViolationType::RateExceeded),
            PolicyErrorCode::RateExceeded
        );
        assert_eq!(
            PolicyErrorCode::from(&ViolationType::PermissionDenied),
            PolicyErrorCode::PermissionDenied
        );
        assert_eq!(
            PolicyErrorCode::from(&ViolationType::QuotaExceeded),
            PolicyErrorCode::QuotaExceeded
        );
        assert_eq!(
            PolicyErrorCode::from(&ViolationType::ApprovalRequired),
            PolicyErrorCode::ApprovalRequired
        );
    }

    #[test]
    fn test_allowed_result_gives_no_error() {
        let r = EvaluationResult::allow();
        assert!(
            PolicyApiError::from_evaluation_result(&r).is_none(),
            "allowed result should not produce an error"
        );
    }

    #[test]
    fn test_deny_result_gives_error() {
        let r = EvaluationResult::deny("p1", ViolationType::RateExceeded, "exceeded");
        let err = PolicyApiError::from_evaluation_result(&r).unwrap();
        assert_eq!(err.error_code().http_status(), 403);
        assert_eq!(err.error_code().error_code(), "POLICY_RATE_EXCEEDED");
    }
}
