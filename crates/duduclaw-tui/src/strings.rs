//! All user-facing fixed text belongs here. Gateway text must be sanitized.
use crate::Error;
pub const STALE: &str = "資料可能過期";
pub const UNKNOWN_COUNT: &str = "?";
pub const ADMIN_ONLY: &str = "此面板需要管理員權限";
pub const EMAIL: &str = "電子郵件：";
pub const PASSWORD: &str = "密碼：";
pub const OTP: &str = "驗證碼（若帳號已綁定通道，驗證碼已送出）：";
pub const SOURCE_APPROVAL: &str = "審核";
pub const SOURCE_GOAL: &str = "目標任務";
pub const SOURCE_QUESTION: &str = "員工提問";
pub const SOURCE_BREAKER: &str = "斷路器";
pub fn error(error: Error) -> &'static str {
    match error {
        Error::InvalidUrl => "閘道網址格式不正確，請使用不含帳密、查詢參數的根網址或 /ws 網址",
        Error::TlsRequired => "遠端閘道必須使用 HTTPS 或 WSS，且憑證必須有效",
        Error::LocalOnly => "自動登入僅限本機閘道",
        Error::Network => "無法連線至閘道，請檢查連線與憑證",
        Error::Protocol => "閘道回應格式不符，請確認版本",
        Error::LoginRequired => {
            "登入失敗或登入已失效，請重新登入；若尚未設定密碼，請先在儀表板設定密碼或啟用本機自動登入"
        }
        Error::SetupRequired => "請先在儀表板設定密碼，或啟用本機自動登入",
        Error::PasswordChangeRequired => "請先在儀表板變更密碼，再重新登入",
        Error::LegacyIdentity => "不支援舊版管理員權杖或系統身分，請使用帳號登入",
        Error::IdentityChanged => "登入身分不一致，請重新登入",
        Error::PromptCancelled => "已取消登入",
        Error::CoreDumpProtection => "無法停用核心傾印，已停止登入以保護憑證",
        Error::Timeout => "閘道回應逾時",
        Error::Disconnected => "閘道連線已中斷，資料可能過期",
        Error::RpcRejected => "閘道拒絕此請求，請確認帳號權限",
        Error::Busy => "請求過多，請稍後再試",
    }
}
