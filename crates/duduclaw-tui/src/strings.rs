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
        Error::Terminal => "無法使用互動終端，請在終端機啟動控制室",
        Error::LocalState => "無法保存本機已讀紀錄，請檢查目錄權限",
        Error::Output => "無法寫入輸出",
    }
}

pub const TITLE: &str = "DuDuClaw 控制室";
pub const INBOX: &str = "收件匣";
pub const EMPLOYEES: &str = "員工";
pub const NEXT_RUNS: &str = "下次自主執行";
pub const DETAIL: &str = "詳情（唯讀）";
pub const DETAIL_HINT: &str = "選取項目後按 Enter 查看詳情";
pub const EMPTY_INBOX: &str = "目前沒有待處理事項";
pub const EMPTY_EMPLOYEES: &str = "目前沒有可見員工";
pub const NO_SCHEDULE: &str = "閘道未提供下次執行時間";
pub const CONNECTING: &str = "連線中";
pub const CONNECTED: &str = "已連線";
pub const DISCONNECTED: &str = "已斷線";
pub const GATEWAY: &str = "閘道";
pub const AUTONOMY: &str = "自主狀態";
pub const LIST_FOCUS: &str = "焦點：清單";
pub const KEY_HINTS: &str = "j/k 移動  Enter 詳情  r 已讀  ? 說明  q 離開";
pub const HELP: &str = "操作說明\n方向鍵 / j k：移動選取\nEnter：開啟唯讀詳情\nr：標為此客戶端已讀\n? / F1：開關說明\nPgUp / PgDn：捲動詳情\nq / Ctrl-C：離開\n\n新項目不會移動選取。\n關閉控制室不會停止員工。\n待處理事項即使已讀仍保留。";
pub const SETTINGS_NOTICE: &str = "設定無法讀取或有無效值，已採用預設值";
pub const READ_STATE_NOTICE: &str = "已讀紀錄無法讀取，從空白紀錄開始";
pub const SAVED: &str = "已標為已讀";
pub const READ: &str = "已讀";
pub const REMOVED: &str = "選取的項目已不在清單，請用方向鍵選取";
pub const PANIC: &str = "控制室發生錯誤，已還原終端；請重新啟動";
pub const WORKING: &str = "執行中";
pub const AWAITING: &str = "待回覆";
pub const DONE: &str = "完成未讀";
pub const IDLE: &str = "閒置";
pub const PAUSED: &str = "已暫停";
pub const ENDED: &str = "已結束";
pub const FAILED: &str = "錯誤";
pub const HEARTBEAT: &str = "心跳";
pub const CRON: &str = "排程";
pub const NO_ID: &str = "此項目沒有可保存的已讀識別碼";
pub const TERMINAL_SMALL: &str = "終端太小，請放大至至少 24 欄、12 列；q 離開";
