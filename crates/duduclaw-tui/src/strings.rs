//! All user-facing fixed text belongs here. Gateway text must be sanitized.
use crate::Error;
pub const STALE: &str = "資料可能過期";
pub const UNKNOWN_COUNT: &str = "?";
pub const ADMIN_ONLY: &str = "此面板需要管理員權限";
pub const EMAIL: &str = "電子郵件";
pub const PASSWORD: &str = "密碼";
pub const OTP: &str = "驗證碼（若帳號已綁定通道，驗證碼已送出）";
pub const MFA_METHOD: &str = "雙因素驗證方式";
pub const MFA_AUTHENTICATOR: &str = "驗證器六位數驗證碼";
pub const MFA_RECOVERY: &str = "復原碼";
pub const MFA_CODE: &str = "驗證器六位數驗證碼";
pub const MFA_RECOVERY_CODE: &str = "復原碼";
pub const MFA_RETRY: &str = "驗證碼不正確，請重新輸入，或改用復原碼";
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
        Error::SnapshotBudget => "清單超過安全容量，資料可能過期；請改用儀表板查看",
        Error::Protocol => "閘道回應格式不符，請確認版本",
        Error::LoginRequired => {
            "登入失敗或登入已失效，請重新登入；若尚未設定密碼，請先在儀表板設定密碼或啟用本機自動登入"
        }
        Error::MfaEnrollmentRequired => "帳號必須先設定雙因素驗證，請到儀表板完成設定後重新登入",
        Error::MfaTerminalRequired => "雙因素驗證需要互動終端，請在終端機重新執行並輸入驗證碼或復原碼",
        Error::MfaFailed => "雙因素驗證未通過、已達嘗試上限或驗證已失效，請重新登入",
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
pub const DETAIL: &str = "詳情";
pub const DETAIL_HINT: &str = "選取項目後按 Enter 查看詳情";
pub const EMPTY_INBOX: &str = "目前沒有待處理事項";
pub const EMPTY_INBOX_GUIDE: &str = "員工需要你時會出現在這裡；? 查看說明";
pub const EMPTY_EMPLOYEES: &str = "尚無可見員工，狀態會顯示於此；? 查看說明";
pub const NO_SCHEDULE: &str = "等待下次排程時間；? 查看說明";
pub const CONNECTING: &str = "連線中";
pub const CONNECTED: &str = "已連線";
pub const DISCONNECTED: &str = "已斷線";
pub const GATEWAY: &str = "閘道";
pub const AUTONOMY: &str = "自主";
pub const AUTONOMY_PAUSED: &str = "已暫停";
pub const AUTONOMY_ALL: &str = "全部暫停";
pub const AUTONOMY_UNKNOWN: &str = "待確認";
pub const AUTONOMY_UNREADABLE: &str = "狀態無法讀取，待確認";
pub const AUTONOMY_ROW_PAUSED: &str = "自主已暫停";
pub const AUTONOMY_ROW_UNKNOWN: &str = "自主狀態待確認";
pub const AUTONOMY_PAUSE: &str = "暫停此員工自主工作";
pub const AUTONOMY_RESUME: &str = "恢復此員工自主工作";
pub const AUTONOMY_PAUSE_ALL: &str = "暫停全部自主工作";
pub const AUTONOMY_RESUME_ALL: &str = "恢復全部自主工作";
pub const AUTONOMY_APPLIED: &str = "閘道已套用，正在重讀狀態";
pub const AUTONOMY_WARNING: &str = "；保存或稽核尚未確認，請檢查閘道";
pub const AUTONOMY_UNCONFIRMED: &str = "自主工作操作結果未確認，請重讀狀態後再決定是否重試";
pub fn autonomy_refusal(reason: &str) -> String {
    let explanation = if reason.contains("permission denied") { "權限不足，需要 Manager 與員工操作權限；全部操作需要 Admin" }
        else if reason.contains("audit unavailable") { "稽核紀錄無法寫入，狀態未變更" }
        else if reason.contains("state unchanged") { "暫停狀態無法寫入，狀態未變更" }
        else if reason.contains("outcome unknown") { return AUTONOMY_UNCONFIRMED.into(); }
        else { "閘道拒絕操作，請確認權限與狀態" };
    format!("自主工作操作遭拒：{explanation}（{reason}）")
}
pub const LIST_FOCUS: &str = "焦點：清單";
pub const LIST_KEY_HINTS: &str = "Enter 開啟  Tab 分頁  I 收件匣  M 通知  z 放大  Ctrl-P 命令  ? 說明  q 離開";
pub struct HelpEntry { pub key: &'static str, pub description: &'static str }
pub struct HelpGroup { pub title: &'static str, pub entries: &'static [HelpEntry] }
pub const HELP: &[HelpGroup] = &[
    HelpGroup { title: "導覽", entries: &[
        HelpEntry { key: "Tab / Shift-Tab", description: "對話／任務／紀錄分頁" },
        HelpEntry { key: "I / M / S", description: "收件匣／通知信箱／下次自主執行；Esc 返回側欄" },
        HelpEntry { key: "信箱 Tab", description: "全部／未讀／錯誤；方向鍵選取；Enter 前往" },
        HelpEntry { key: "F8", description: "選取通知；x 關閉；Esc 返回" },
        HelpEntry { key: "D/C/R/T/L", description: "詳情／對話／執行／任務／日誌" },
        HelpEntry { key: "方向鍵 / j k", description: "移動選取" },
        HelpEntry { key: "PgUp / PgDn", description: "捲動詳情" },
        HelpEntry { key: "t", description: "時間軸焦點；方向鍵選取；Enter 展開／收合；Esc 返回" },
    ] },
    HelpGroup { title: "員工與工作", entries: &[
        HelpEntry { key: "i / /", description: "輸入訊息；Esc 返回清單" },
        HelpEntry { key: "[ / ]", description: "前後紀錄；b 切換任務／責任" },
        HelpEntry { key: "p / u", description: "立即暫停／恢復選取員工的自主工作" },
        HelpEntry { key: "p / n", description: "責任或 cron 暫停／cron 或心跳執行（確認卡）" },
        HelpEntry { key: "Ctrl-P", description: "命令面板：全部自主工作暫停／恢復（Admin）、員工狀態操作（確認卡）" },
        HelpEntry { key: "X", description: "取消回覆（需閘道支援）" },
        HelpEntry { key: "r", description: "標為此客戶端已讀" },
        HelpEntry { key: "s", description: "任務區塊停止；其他區塊為本機稍後提醒（最多十分鐘）" },
        HelpEntry { key: "W/A/E/B/G/N", description: "流程／自動規則／演化／預算／摘要／建立目標" },
    ] },
    HelpGroup { title: "決策卡與確認", entries: &[
        HelpEntry { key: "Enter", description: "開啟詳情或決策卡" },
        HelpEntry { key: "防誤觸", description: "卡片開啟後一秒內忽略按鍵" },
        HelpEntry { key: "Enter / n", description: "核准須輸入兩位確認碼後按 Enter；n 拒絕" },
        HelpEntry { key: "高風險卡", description: "須先捲到底部" },
        HelpEntry { key: "目標 r", description: "重試；待核准計畫須捲到底並輸入確認碼" },
        HelpEntry { key: "目標 a/d/t", description: "中止／完成／接手須連按兩次" },
        HelpEntry { key: "Esc", description: "關卡；新項目不會移動焦點" },
    ] },
    HelpGroup { title: "區塊與分割", entries: &[
        HelpEntry { key: "1–4", description: "區塊焦點" },
        HelpEntry { key: "v / x", description: "左右／上下分割" },
        HelpEntry { key: "Ctrl-W", description: "關閉區塊" },
        HelpEntry { key: "z", description: "放大目前區塊／還原" },
    ] },
    HelpGroup { title: "本機終端", entries: &[
        HelpEntry { key: "P", description: "開啟本機終端" },
        HelpEntry { key: "Enter / i", description: "輸入終端" },
        HelpEntry { key: "Ctrl-\\ / F12", description: "返回控制室" },
        HelpEntry { key: "PgUp / PgDn", description: "清單焦點時捲動（最多 5,000 行）" },
        HelpEntry { key: "關閉", description: "關閉區塊或控制室會停止本機終端行程" },
    ] },
    HelpGroup { title: "其他", entries: &[
        HelpEntry { key: "? / F1", description: "開關說明" },
        HelpEntry { key: "Ctrl-P", description: "命令面板（Ctrl-K 保留）" },
        HelpEntry { key: "q / Ctrl-C", description: "離開；關閉控制室不會停止員工" },
    ] },
];
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

pub const AI_SESSION_REFUSED: &str = "控制室只供操作者使用，不能在 AI 員工工作階段中啟動";
pub const LOGIN_METHOD: &str = "選擇登入方式（已啟用雙因素驗證的帳號還需第二步驗證）";
pub const LOGIN_PASSWORD: &str = "帳號密碼";
pub const LOGIN_EMAIL_CODE: &str = "電子郵件帳號驗證碼";
pub const CARD_TITLE: &str = "決策卡";
pub const CARD_FOCUS: &str = "焦點：決策卡";
pub const CARD_WAIT: &str = "卡片剛開啟，請稍候一秒再操作";
pub const CARD_CODE: &str = "核准確認碼：";
pub const CARD_APPROVAL_KEYS: &str = "輸入確認碼後按 Enter 核准；n 拒絕";
pub const CARD_GOAL_KEYS: &str = "r 重試；a 中止 / d 完成 / t 接手（同鍵再按一次）";
pub const CARD_CONTINUE_KEYS: &str = "c 輸入接續訊息";
pub const CARD_NAV: &str = "↑↓ / PgUp PgDn 捲動；s 稍後；Esc 關卡";
pub const CARD_NAV_NO_SNOOZE: &str = "↑↓ / PgUp PgDn 捲動；Esc 關卡";
pub const CARD_SCROLL: &str = "高風險操作：請先捲動完整內容到底部；n 可拒絕";
pub const CARD_WRONG_CODE: &str = "確認碼不符，請重新輸入";
pub const CARD_SECOND_PRESS: &str = "請再按一次相同按鍵確認；其他按鍵取消確認";
pub const CARD_READ_ONLY: &str = "此卡片唯讀";
pub const CARD_NO_PERMISSION: &str = "目前身分沒有此決策權限，或閘道尚未提供可用權限";
pub const CARD_DASHBOARD: &str = "此事項只能在儀表板處理，請自行開啟儀表板";
pub const CARD_NOT_GOAL: &str = "此項目不是可決策的目標任務，請到儀表板處理";
pub const CARD_NOT_PENDING: &str = "此任務目前沒有可用決策";
pub const CARD_EXPIRED: &str = "審核已到期，不能操作；到期即視為拒絕";
pub const CARD_NO_DEADLINE: &str = "審核非待處理狀態或缺少有效期限，不能操作";
pub const CARD_CHANGED: &str = "卡片內容已變更，已關閉並重新取得清單，請重新開啟確認";
pub const CARD_REOPEN: &str = "卡片內容已變更，請關閉後重新開啟確認";
pub const DECISION_REFETCH: &str = "正在重新取得清單，請以閘道最新狀態為準";
pub const DECISION_SENDING: &str = "正在送出決策，請稍候";
pub const SNOOZED: &str = "已在本機稍後提醒；不會延長閘道期限";
pub const SNOOZE_UNAVAILABLE: &str = "此事項已到期或缺少有效期限，無法稍後提醒";
pub const RESURFACED: &str = "稍後提醒已結束或期限已變更；審核到期即拒絕";

pub const CARD_UNBOUND: &str = "內容未綁定：這張卡無法確認你看到的就是實際內容，請到儀表板決定";
pub const CARD_DENY_KEY: &str = "n 拒絕";
pub const CARD_NO_REVISION: &str = "缺少任務版本，請重新開卡或到儀表板決定";
pub const CARD_KEY_HINTS: &str = "↑↓ / PgUp PgDn 捲動  s 稍後  Esc 關卡  Ctrl-C 離開；決策鍵見卡片";
pub const HELP_KEY_HINTS: &str = "? / F1 關閉說明  Esc 返回清單  q / Ctrl-C 離開";
pub const HELP_FOCUS: &str = "焦點：說明";


pub const INPUT_FOCUS: &str = "焦點：輸入";
pub const INPUT_HINT: &str = "Enter 傳送  Esc 返回清單";
pub const INPUT_NEEDS_AGENT: &str = "請選取員工，或輸入 @員工名稱 加上訊息";
pub const COMPUTER_COMMAND_NOTE: &str = "這是電腦操作指令，不會暫停員工";
pub const CHAT_TITLE: &str = "對話 [2]";
pub const EMPTY_CHAT: &str = "員工回覆會顯示於此；i 輸入訊息，Enter 傳送";
pub const CHAT_USER: &str = "你";
pub const CHAT_ASSISTANT: &str = "員工";
pub const CHAT_SYSTEM: &str = "系統";
pub const CHAT_STEP: &str = "工具步驟";
pub const CHAT_THINKING: &str = "思考中（已收合）";
pub const CHAT_RESPONDING: &str = "正在回覆";
pub const CHAT_ERROR: &str = "對話發生錯誤";
pub const CHAT_TURN_LIMIT: &str = "同時進行的對話已達上限，請稍後再傳送";
pub const CHAT_CANCEL_UNAVAILABLE: &str = "此閘道尚未確認支援取消，無法取消";
pub const CHAT_CANCEL_HINT: &str = "X 取消這次回覆";
pub const CHAT_CANCELLING: &str = "已接受取消，等待停止確認";
pub const CHAT_CANCELLED: &str = "回覆已取消";
pub const DOWNSTREAM_INPUT_HINT: &str = "Esc 返回清單後以 y／n 回答；輸入區的 y／n 只會輸入文字";
pub const DOWNSTREAM_STOPPING: &str = "正在停止下游工作";
pub const DOWNSTREAM_UNKNOWN: &str = "這個詢問已失效，下游工作不受影響";
pub const CMD_DOWNSTREAM_STOP: &str = "一起停止這次回覆的下游工作";
pub const CMD_DOWNSTREAM_DISMISS: &str = "讓下游工作跑完";
pub fn downstream_prompt(count: u64) -> String {
    format!("還有 {count} 位員工的工作還在跑，要一起停嗎？ y 停止／n 讓它們跑完")
}
pub fn downstream_prompt_compact(count: u64) -> String {
    format!("y 停止／n 繼續：{count} 位員工在跑")
}
pub fn downstream_stopped(stopped: u64, still_stopping: u64) -> String {
    let mut text = format!("已停止 {stopped} 項");
    if still_stopping > 0 { text.push_str(&format!("，{still_stopping} 項還在收尾")); }
    text
}
pub const CHAT_INTERRUPTED: &str = "連線中斷，回覆結果未確認；不會自動重送訊息";
pub const NAVIGATE_INBOX_HINT: &str = "收件匣有待處理項目，請選取查看";
pub const WATCH_AGENT_POLLING: &str = "輪詢模式：每五秒更新員工執行紀錄";
pub const WATCH_POLLING: &str = "輪詢模式：閘道未提供即時訂閱，每五秒更新";
pub const WATCH_SUBSCRIBED: &str = "已訂閱員工執行事件";
pub const WATCH_GAP: &str = "事件可能遺失，正在重新取得執行紀錄";
pub const WATCH_RUN_MISMATCH: &str = "警告：執行紀錄與目標不符，已略過";
pub fn notification(count: usize) -> String { format!("DuDuClaw：{count} 項需要你處理") }

pub const WATCH_HELP: &str = "持續查看員工或 --run 指定的執行紀錄";

pub const WATCH_TRUNCATED: &str = "較早事件已省略，每次執行僅顯示最近 256 筆";
pub const CHAT_DONE: &str = "回覆完成";

pub const WATCH_CONTENT_HELP: &str = "顯示未遮蔽的內容預覽";
pub const WATCH_CONTENT_WARNING: &str = "警告：內容未經機密遮蔽，可能包含敏感資料。";
pub const CHAT_GUTTER: &str = "> ";

pub const CHAT_BLOCK: &str = "對話";
pub const RUN_BLOCK: &str = "執行紀錄";
pub const TASK_BLOCK: &str = "任務";
pub const LOG_BLOCK: &str = "日誌";
pub const NOT_PROVIDED: &str = "閘道未提供";
pub const MANAGER_ONLY: &str = "需要主管權限";
pub const CRON_ADMIN_ONLY: &str = "排程需要管理員權限；此處僅顯示心跳";
pub const RUNTIME_ENDPOINTS: &str = "此 runtime 只回報起訖";
pub const SELECT_EMPLOYEE: &str = "請先選取員工，再開啟執行或任務區塊";
pub const CONTENT_CONFIRM: &str = "警告：預覽僅截短，未遮蔽機密；再按 c 顯示內容，其他鍵取消。";
pub const RUN_KEYS: &str = "[ / ] 切換紀錄；c 顯示／隱藏內容";
pub const ROW_KEYS: &str = "[ / ] 切換；b 任務／責任";
pub const TASK_KEYS: &str = "[ / ] 切換；b 任務／責任；Enter 查看目標卡";
pub const RESP_LIST: &str = "持續責任";
pub const RESP_KEYS: &str = "p 暫停；d 停用（均需確認）";
pub const RESP_OCCURRENCE: &str = "補充方向或停止請切回任務，選取這次責任的執行任務";
pub const ITERATIONS: &str = "回合";
pub const TIMELINE: &str = "時間軸";
pub const CHANGES: &str = "檔案變更";
pub const BLOCK_TRUNCATED: &str = "部分資料已省略，請到儀表板查看";
pub const LOG_GAP: &str = "僅保留最近 256 行；較早或遺失的日誌無法重播";
pub const STEER: &str = "補充任務方向";
pub const STOP: &str = "停止任務與子任務";
pub const EMPLOYEE_PAUSE: &str = "暫停員工狀態（需確認）";
pub const EMPLOYEE_RESUME: &str = "恢復員工狀態（需確認）";
pub const CRON_PAUSE: &str = "暫停排程";
pub const CRON_RESUME: &str = "恢復排程";
pub const RUN_NOW: &str = "立即執行一次";
pub const RESP_DISABLE: &str = "停用持續責任";
pub const RESP_PAUSE: &str = "暫停持續責任";
pub const CONTROL_SCOPE: &str = "請核對操作與目標；送出後會重新取得狀態。Esc 取消。";
pub const CONTROL_SCROLL: &str = "請先捲到底部核對完整內容。Esc 取消。";
pub const CONTROL_CONFIRM: &str = "輸入兩位確認碼後按 Enter；Esc 取消。";
pub const STEER_EDIT: &str = "輸入方向（最多 4000 字）；Enter 進入確認；Esc 取消。";
pub const EMPLOYEE_KEYS: &str = "p 暫停自主工作；u 恢復自主工作（立即送出）；員工狀態操作在 Ctrl-P";
pub const HEARTBEAT_KEYS: &str = "n 觸發心跳（需確認）";
pub const CRON_KEYS: &str = "p 暫停／恢復；n 立即執行（需確認）";
pub fn hidden_blocks(count: usize) -> String { format!("另有 {count} 個區塊隱藏；1–4 切換焦點") }
pub const OPEN_OCCURRENCE: &str = "o 開啟此次執行任務（可補充方向、停止）";
pub const TASK_STEER_KEY: &str = "e 補充方向（需確認）";
pub const TASK_STOP_KEY: &str = "s 停止（需確認）";
pub fn field_label(key: &str) -> &str {
    match key {
        "responsibility_id" => "責任", "objective" => "目標", "title" => "標題", "status" | "state" => "狀態", "assigned_to" | "owner_agent_id" => "員工",
        "pause_reason" => "暫停原因", "updated_at" => "更新時間", "name" => "名稱", "stop_at" => "截止時間",
        "last_run" | "last_run_at" => "上次執行", "last_status" => "上次結果", "cron" => "排程規則",
        "round" => "回合", "role" => "角色", "runtime" => "執行環境", "provider" => "供應商",
        "outcome" => "結果", "timestamp" | "ts" => "時間", "handle" => "條件", "text" => "內容",
        "evidence" => "證據", "unresolved" => "未解決", "updated_round" => "更新回合", "step_id" => "步驟",
        "event" => "事件", "scope" => "範圍", "spent_cents" => "支出（美分）", "cap_cents" => "上限（美分）",
        "gvu_enabled" => "演化啟用", "cognitive_memory" => "認知記憶", "is_stagnant" => "停滯",
        "summary" => "摘要", "checked_at" => "檢查時間", "category" => "類別", "content" => "內容",
        "finished_total" => "已完成", "pending_approvals" => "待核准", "runs_done" => "完成執行",
        "runs_not_done" => "未完成執行", "spend_usd" => "費用（美元）", "completed_at" => "完成時間", "produced_at" => "交付時間",
        "agent_id" => "員工", "id" => "識別碼", "run_id" => "執行識別碼", "workflow_id" => "流程",
        "error_code" => "錯誤", "created_at" => "建立時間", "deadline_at" => "截止時間",
        "opened_at" => "開啟時間", "trigger_event" => "觸發事件", "mode" => "模式",
        "total_rounds" => "累計回合", "applied_rounds" => "已套用回合", "date" => "日期",
        "since" => "起始時間", "generated_at" => "產生時間", "last_report_round" => "最近回報回合",
        "invalid_reports" => "無效回報", "plan_pending" => "待核准計畫",
        "cron_timezone" => "時區", "enabled" => "啟用", _ => key,
    }
}

pub const LAST_RESULT: &str = "上次結果";

pub const RUN_WAIT: &str = "正在確認執行紀錄與訂閱能力";

pub const RUN_LIMIT: &str = "只顯示最近 50 次執行";
pub const EMPTY_RUNS: &str = "等待員工執行，紀錄會顯示於此；選取員工後按 R 查看";
pub const EMPTY_TASKS: &str = "任務會顯示於此；選取員工後按 T 查看";
pub const EMPTY_RESPONSIBILITIES: &str = "持續責任會顯示於此；b 切換任務，Ctrl-P 查看命令";
pub const EMPTY_LOG: &str = "等待閘道事件，日誌會顯示於此；Ctrl-P 查看命令";

pub const CONTROL_KEY_HINTS: &str = "↑↓ / PgUp PgDn 捲動  Esc 關卡  Ctrl-C 離開；確認方式見卡片";

pub const PALETTE_FOCUS: &str = "焦點：命令面板";
pub const PALETTE_TITLE: &str = "命令面板";
pub const PALETTE_KEYS: &str = "輸入搜尋  ↑/↓ 選取  Enter 執行  Esc 返回";
pub const PALETTE_EMPTY: &str = "沒有符合的可用命令";
pub const CMD_QUIT: &str = "離開控制室";
pub const CMD_BACK: &str = "返回清單／關閉說明";
pub const CMD_SEND: &str = "傳送訊息";
pub const CMD_HELP: &str = "開關此區塊的說明";
pub const CMD_UP: &str = "向上捲動";
pub const CMD_DOWN: &str = "向下捲動";
pub const CMD_DETAIL: &str = "開啟詳情區塊";
pub const CMD_CHAT: &str = "開啟對話區塊";
pub const CMD_INPUT: &str = "輸入對話訊息";
pub const CMD_RUN: &str = "查看選取員工的執行紀錄";
pub const CMD_TASK: &str = "查看選取員工的任務";
pub const CMD_LOG: &str = "查看日誌";
pub const CMD_VERTICAL: &str = "左右分割區塊";
pub const CMD_HORIZONTAL: &str = "上下分割區塊";
pub const CMD_CLOSE: &str = "關閉目前區塊";
pub const CMD_FOCUS: &str = "切換區塊焦點";
pub const CMD_PREVIOUS: &str = "前一筆紀錄";
pub const CMD_NEXT: &str = "下一筆紀錄";
pub const CMD_RESP: &str = "切換任務／持續責任";
pub const CMD_CONTENT: &str = "切換執行內容顯示";
pub const CMD_CANCEL: &str = "取消目前回覆";
pub const CMD_OPEN: &str = "開啟詳情／決策卡";
pub const CMD_READ: &str = "標為已讀";
pub const CMD_SNOOZE: &str = "本機稍後提醒";
pub const CMD_OCCURRENCE: &str = "開啟本次責任任務";
pub const CMD_STEER: &str = "調整任務方向（需確認）";
pub const CMD_STOP: &str = "停止任務（需確認）";
pub const CMD_PAUSE: &str = "暫停／恢復（需確認）";
pub const CMD_DISABLE: &str = "停用持續責任（需確認）";
pub const CMD_NOW: &str = "立即執行（需確認）";
pub const EXIT_PENDING: &str = "待處理收件匣";
pub const EXIT_RUNNING: &str = "執行中的員工";
pub const EXIT_RETURN: &str = "返回控制室";
pub const EXIT_NONE: &str = "無";

pub fn exit_more(count: usize) -> String { format!("另 {count} 位") }


pub const PTY_BLOCK: &str = "本機終端";
pub const PTY_COMMAND_IGNORED: &str = "舊版本機終端命令設定已忽略，改用啟動控制室者的 shell；其他無效設定採預設值";
pub const PTY_STARTING: &str = "本機終端準備中";
pub const PTY_RUNNING: &str = "本機行程執行中";
pub const PTY_EXITED: &str = "本機行程已結束，代碼：";
pub const PTY_STOPPED: &str = "本機行程已停止";
pub const PTY_STOP_FAILED: &str = "本機行程清理未完整確認";
pub const PTY_DISABLED: &str = "此平台尚未驗證終端行程樹清理，本機終端已停用";
pub const PTY_START_FAILED: &str = "無法啟動本機終端，請檢查啟動控制室時的 shell 環境";
pub const PTY_RESIZE_FAILED: &str = "無法調整本機終端大小";
pub const PTY_INPUT_FAILED: &str = "輸入寫入失敗：行程已結束或終端無法寫入，請檢查命令";
pub const PTY_INPUT_FULL: &str = "整段輸入未送出：待寫入資料超過 1 MiB";
pub const PTY_FOCUS: &str = "本機終端；Ctrl-\\ / F12 返回控制室";
pub const PTY_KEYS: &str = "Ctrl-\\ / F12 返回控制室；其他按鍵送至本機行程";
pub const PTY_LIST_KEYS: &str = "Enter / i 輸入；PgUp / PgDn 捲動；Ctrl-W 關閉";
pub const PTY_CONFIRM: &str = "本機行程仍在執行，關閉會停止同一終端的行程；刻意脫離的行程會繼續執行。y 確認；Esc 取消";
pub const PTY_CONFIRM_FOCUS: &str = "確認關閉本機終端";
pub const CMD_PTY: &str = "開啟本機終端";


pub const WORKFLOW_BLOCK: &str = "工作流程";
pub const AUTOPILOT_BLOCK: &str = "自動規則";
pub const EVOLUTION_BLOCK: &str = "演化與經驗";
pub const BUDGET_BLOCK: &str = "預算事件";
pub const DIGEST_BLOCK: &str = "最新摘要";
pub const VIEW_EMPTY: &str = "沒有資料不代表系統沒有異常；可能是權限或篩選範圍。";
pub const EMPTY_WORKFLOW: &str = "工作流程執行會顯示於此；Ctrl-P 查看命令";
pub const EMPTY_AUTOPILOT: &str = "自動規則與斷路器狀態會顯示於此；Ctrl-P 查看命令";
pub const EMPTY_EVOLUTION: &str = "演化狀態與經驗會顯示於此；Ctrl-P 查看命令";
pub const EMPTY_BUDGET: &str = "預算事件會顯示於此；Ctrl-P 查看命令";
pub const EMPTY_DIGEST: &str = "最新工作摘要會顯示於此；Ctrl-P 查看命令";
pub const VIEW_READ_ONLY: &str = "唯讀；完整內容請到儀表板查看";
pub const VIEW_UNAVAILABLE: &str = "目前無法取得資料";
pub const SELECT_TASK: &str = "請先在任務區塊選取任務，再開啟工作流程";
pub const WORKFLOW_KEYS: &str = "[ / ] 切換執行；s 取消流程（確認卡）；完整清單請到儀表板";
pub const WORKFLOW_CANCEL: &str = "取消工作流程執行";
pub const ROLES: &str = "團隊角色回合";
pub const CRITERIA: &str = "驗收條件紀錄";
pub const BREAKERS: &str = "斷路器狀態";
pub const STAGNATION: &str = "停滯訊號";
pub const PLAYBOOK: &str = "經驗條目";
pub const NOT_SCHEDULED: &str = "未排定或未知";
pub const GOAL_CREATE: &str = "建立自主目標";
pub const GOAL_EDIT: &str = "輸入目標；Tab 切換先擬計畫；Enter 核對；Esc 取消";
pub const PLAN_FIRST: &str = "先擬計畫";
pub const PLAN_APPROVE: &str = "核准計畫";
pub const PLAN_KEYS: &str = "核准計畫：捲到底部，輸入確認碼後按 Enter；a 中止（連按兩次）";
pub const CONTINUE_OPEN: &str = "c 輸入接續訊息";
pub const CONTINUE_EDIT: &str = "輸入接續訊息；Enter 核對；Esc 清除並關閉";
pub const BREAKER_EVENT: &str = "曾收到斷路器開啟事件；Enter 查看目前資料（非決策卡）";
pub fn working_runs(count: Option<usize>) -> String {
    format!("執行中 {} 次（已觀測）", count.map(|n| n.to_string()).unwrap_or_else(|| "?".into()))
}
pub fn cost_window(millicents: u64) -> String { format!("近 24 小時 ${}", cost_amount(millicents)) }
pub fn run_round_title(source: &str, round: i64) -> String { format!("{source} · 回合 {round}") }
pub const GLOBAL_HINT_SUFFIX: &str = "；Ctrl-P 命令面板；q 離開";

pub const YES: &str = "是";
pub const NO: &str = "否";

pub const WORKING_TASK_UNKNOWN: &str = "此次執行的任務未知；以下是員工的任務清單，並非執行連結";

pub const WATCH_RUN_HELP: &str = "追蹤指定執行紀錄；不可與員工名稱同時使用";
pub const NOTIFY_CONNECTION_ERROR: &str = "DuDuClaw：連線中斷，請檢查控制室";
pub const PTY_WAITING: &str = "? 等待你的輸入（僅提示）";
pub fn cost_today(millicents: u64) -> String { format!("今日 ${}", cost_amount(millicents)) }
pub const CONTINUE_REVIEW: &str = "核對接續訊息（全文）；捲動到底後輸入確認碼，再按 Enter 送出";
pub fn inbox_information(count: usize) -> String { format!("資訊 {count}") }

fn cost_amount(millicents: u64) -> String {
    let amount = millicents as f64 / 100_000.0;
    if millicents > 0 && millicents < 1000 { format!("{amount:.4}") }
    else { format!("{amount:.2}") }
}

/// Add spacing only at a CJK/ASCII name boundary.
pub fn name_boundary(name: &str, before: bool) -> String {
    let ascii = if before { name.chars().next() } else { name.chars().next_back() }
        .is_some_and(|c| c.is_ascii_alphanumeric());
    if !ascii { name.into() } else if before { format!(" {name}") } else { format!("{name} ") }
}
pub fn composer_placeholder(name: &str) -> String { format!("輸入訊息給{}…", name_boundary(name, true)) }
