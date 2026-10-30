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
        Error::SnapshotBudget => "清單超過安全容量，資料可能過期；請改用儀表板查看",
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
pub const DETAIL: &str = "詳情";
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
pub const CONTROL_KEY_FOCUS: &str = "p / n 僅在詳情區塊或清單焦點可用";
pub const LIST_KEY_HINTS: &str = "j/k 選取  1–4 焦點  v/x 分割  Ctrl-W 關閉  ? 說明";
pub const HELP: &str = "操作說明\ni / /：輸入訊息；Esc 返回清單\n1–4：區塊焦點；v/x：左右／上下分割；Ctrl-W：關閉\nD/C/R/T/L：詳情／對話／執行／任務／日誌\n[ / ]：前後紀錄；b：任務／責任；p：員工或排程暫停\nX：取消回覆（需閘道支援）\n方向鍵 / j k：移動選取\nEnter：開啟詳情或決策卡\nr：標為此客戶端已讀\ns：任務區塊停止；其他區塊為本機稍後提醒（最多十分鐘）\n? / F1：開關說明\nPgUp / PgDn：捲動詳情\nCtrl-P：命令面板（Ctrl-K 保留）\nq / Ctrl-C：離開\n\n卡片開啟後一秒內忽略按鍵。\n核准：輸入兩位確認碼後按 Enter；n 拒絕。\n高風險卡須先捲到底部。\n目標：r 重試；a 中止、d 完成、t 接手須連按兩次。\nEsc：關卡。新項目不會移動焦點。\n關閉控制室不會停止員工。";
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
pub const LOGIN_METHOD: &str = "選擇登入方式（擇一即可，驗證碼不是雙因素驗證）";
pub const LOGIN_PASSWORD: &str = "帳號密碼";
pub const LOGIN_EMAIL_CODE: &str = "電子郵件帳號驗證碼";
pub const CARD_TITLE: &str = "決策卡";
pub const CARD_FOCUS: &str = "焦點：決策卡";
pub const CARD_WAIT: &str = "卡片剛開啟，請稍候一秒再操作";
pub const CARD_CODE: &str = "核准確認碼：";
pub const CARD_APPROVAL_KEYS: &str = "輸入確認碼後按 Enter 核准；n 拒絕";
pub const CARD_GOAL_KEYS: &str = "r 重試；a 中止 / d 完成 / t 接手（同鍵再按一次）";
pub const CARD_CONTINUE_KEYS: &str = "i 輸入接續訊息，Esc 結束輸入；c 接著做";
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
pub const CHAT_INTERRUPTED: &str = "連線中斷，回覆結果未確認；不會自動重送訊息";
pub const WATCH_POLLING: &str = "輪詢模式：閘道未提供即時訂閱，每五秒更新";
pub const WATCH_SUBSCRIBED: &str = "已訂閱員工執行事件";
pub const WATCH_GAP: &str = "事件可能遺失，正在重新取得執行紀錄";
pub const WATCH_RUN_MISMATCH: &str = "警告：執行紀錄與目標不符，已略過";
pub fn notification(count: usize) -> String { format!("DuDuClaw：{count} 項需要你處理") }

pub const WATCH_HELP: &str = "持續查看一位員工的執行紀錄";

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
pub const EMPLOYEE_PAUSE: &str = "暫停員工狀態（agents.pause，非自主性全面暫停）";
pub const EMPLOYEE_RESUME: &str = "恢復員工狀態（agents.resume）";
pub const CRON_PAUSE: &str = "暫停排程";
pub const CRON_RESUME: &str = "恢復排程";
pub const RUN_NOW: &str = "立即執行一次";
pub const RESP_DISABLE: &str = "停用持續責任";
pub const RESP_PAUSE: &str = "暫停持續責任";
pub const CONTROL_SCOPE: &str = "請核對操作與目標；送出後會重新取得狀態。Esc 取消。";
pub const CONTROL_SCROLL: &str = "請先捲到底部核對完整內容。Esc 取消。";
pub const CONTROL_CONFIRM: &str = "輸入兩位確認碼後按 Enter；Esc 取消。";
pub const STEER_EDIT: &str = "輸入方向（最多 4000 字）；Enter 進入確認；Esc 取消。";
pub const EMPLOYEE_KEYS: &str = "p 暫停／恢復員工狀態（非全面自主暫停，需確認）";
pub const HEARTBEAT_KEYS: &str = "n 觸發心跳（需確認）；上次結果：閘道未提供";
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
        "cron_timezone" => "時區", "enabled" => "啟用", _ => key,
    }
}

pub const LAST_RESULT: &str = "上次結果";

pub const RUN_WAIT: &str = "正在確認執行紀錄與訂閱能力";

pub const RUN_LIMIT: &str = "只顯示最近 50 次執行";
pub const EMPTY_RUNS: &str = "目前沒有可見的執行紀錄";

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
