//! User-facing zh-TW text for each protection state. No file names or
//! internal jargon (dashboard copy rule); the one command named is the one
//! the person has to run.

use super::{BackendKind, ProtectionLevel, ProtectionState};

pub const LEVEL_TOO_LOW: &str =
    "這台機器的憑證保護等級不夠，不能讓 AI 員工使用個人的帳號。";

/// Label for a level, used after 「憑證保護：」.
pub fn level_label(level: ProtectionLevel) -> &'static str {
    match level {
        ProtectionLevel::File => "檔案",
        ProtectionLevel::UserStore => "系統金鑰圈（同一個帳號的程式讀得到）",
        ProtectionLevel::ProcessIsolated => "獨立金鑰（AI 員工讀不到）",
        ProtectionLevel::AppBound => "系統金鑰圈（只有 DuDuClaw 程式讀得到）",
    }
}

/// `(summary, detail)` for a state. `level` is the effective level (only
/// meaningful when `Ready`); `extra` is a short diagnostic for `Unavailable`
/// and `Invalid`.
pub fn state_text(
    state: ProtectionState,
    level: ProtectionLevel,
    backend: BackendKind,
    extra: &str,
) -> (String, String) {
    let summary = |s: &str| format!("憑證保護：{s}");
    if backend == BackendKind::ContainerRootKey {
        let text = match state {
            ProtectionState::NotEnabled => Some((level_label(ProtectionLevel::File), "個人帳號的憑證保護尚未開啟。請到「安全」頁開啟憑證保護，再讓 AI 員工使用個人的 Google 帳號。".to_string())),
            ProtectionState::Missing => Some(("保護金鑰不見了", "找不到加密保存的保護金鑰，可能被刪除了。已保存的個人帳號憑證不能再用。請重新啟用，並請已連結的人重新連結。".to_string())),
            ProtectionState::KeyChanged => Some(("保護金鑰被更換", "加密保存的保護金鑰無法驗證，可能被改動；根金鑰遺失或被更換也會造成這個狀況。個人帳號已停用。請先檢查部署設定和根金鑰，再重新啟用並請大家重新連結。懷疑金鑰外洩時，請依部署指南更換儲存位置中的根金鑰；只在儀表板更換保護金鑰，擋不住有人還原舊的資料夾內容。".to_string())),
            ProtectionState::Unavailable => Some(("獨立金鑰無法使用", match refusal_text(extra) {
                Some(text) => format!("無法使用啟動時交付的獨立金鑰。個人帳號暫停使用；其他功能不受影響。{text}"),
                None => with_extra("無法使用啟動時交付的獨立金鑰。個人帳號暫停使用；其他功能不受影響。", extra),
            })),
            _ => None,
        };
        if let Some((label, detail)) = text { return (summary(label), detail); }
    }
    match state {
        ProtectionState::NotEnabled => (
            summary(level_label(ProtectionLevel::File)),
            "AI 員工的憑證和解密用的金鑰都放在 DuDuClaw 資料夾的檔案裡。要讓 AI 員工使用個人的 Google 帳號，請先把憑證保護改放系統金鑰圈。".into(),
        ),
        ProtectionState::Ready => (summary(level_label(level)), ready_detail(level, backend).into()),
        ProtectionState::NeedsAuthorization => (
            summary("需要重新授權"),
            "系統金鑰圈鎖著，或 DuDuClaw 更新後 macOS 還沒允許新版本讀取。請在這台 Mac 的終端機執行 duduclaw credentials authorize，依畫面指示允許。授權前，個人帳號暫停使用；其他功能不受影響。".into(),
        ),
        ProtectionState::Missing => (
            summary("保護金鑰不見了"),
            "系統金鑰圈裡找不到 DuDuClaw 的保護金鑰，可能被刪除了。已保存的個人帳號憑證不能再用。請重新啟用，並請已連結的人重新連結。".into(),
        ),
        ProtectionState::KeyChanged => (
            summary("保護金鑰被更換"),
            "系統金鑰圈裡的保護金鑰和啟用時的不一樣，可能有程式改動過。個人帳號已停用。請先檢查哪些 AI 員工能執行指令，再重新啟用並請大家重新連結。".into(),
        ),
        ProtectionState::Invalid => (
            summary("設定錯誤"),
            with_extra("憑證保護的記錄損壞，或和這台機器、這個資料夾不符。個人帳號已停用。請重新啟用。", extra),
        ),
        ProtectionState::Unsupported => (
            summary("檔案（尚未啟用隔離金鑰）"),
            "一般 Docker 或 Linux 安裝沒有 AI 員工讀不到的金鑰存放處，因此個人帳號維持停用。Docker 可使用部署指南的獨立金鑰啟動模式；原生 Linux 請依 Linux 受保護服務指南，先驗證發行檔，再從 root 管理的位置安裝。在容器終端機查詢也會顯示此狀態，請以儀表板顯示的實際狀態為準。".into(),
        ),
        ProtectionState::Unavailable => (
            summary("系統金鑰圈無法使用"),
            with_extra("暫時讀不到系統金鑰圈。個人帳號暫停使用；其他功能不受影響。", extra),
        ),
    }
}

fn ready_detail(level: ProtectionLevel, backend: BackendKind) -> &'static str {
    match (level, backend) {
        (ProtectionLevel::ProcessIsolated, _) => "個人帳號的憑證以獨立金鑰保護，只有受保護服務的啟動步驟（root）能讀取根金鑰，並在啟動時交給 DuDuClaw 主程式一次。主機必須禁止 duduclaw 帳號使用容器引擎或其他高權限代理；原生 Linux 模式會在啟動時及每分鐘檢查常見容器引擎介面。符合這些條件時，能執行指令的 AI 員工無法直接讀取或替換根金鑰，但仍能在允許的範圍內要求 DuDuClaw 代用 Google、讀取該回合取得的存取權杖，也能讓 DuDuClaw 停止運作。控制 Linux 主機、Docker 主機或根金鑰儲存位置的人能讀取根金鑰。懷疑金鑰外洩時，請依部署指南更換儲存位置中的根金鑰；只在儀表板更換保護金鑰，擋不住有人還原舊的資料夾內容。",
        (ProtectionLevel::AppBound, _) => "個人帳號的憑證用一把只存在這台 Mac 金鑰圈的金鑰加密，其他程式要讀時 macOS 會擋下。能執行指令的 AI 員工仍可能換掉這把金鑰；gateway 執行期間換掉、或舊的封存資料對不上時，DuDuClaw 會停用個人帳號。",
        (ProtectionLevel::UserStore, BackendKind::MacosKeychain) => "個人帳號的憑證用一把只存在這台 Mac 金鑰圈的金鑰加密，一般程式讀不到。但這個 DuDuClaw 執行檔沒有開啟 hardened runtime，能執行指令的 AI 員工可以把程式碼注入 DuDuClaw 後讀到這把金鑰。",
        (ProtectionLevel::UserStore, _) => "個人帳號的憑證用一把存在 Windows 認證管理員的金鑰加密，不會出現在檔案或備份裡。同一個 Windows 帳號下的程式（包括能執行指令的 AI 員工）仍讀得到。",
        (ProtectionLevel::File, _) => "",
    }
}

/// Closed attestation diagnostics; do not show implementation codes as prose.
pub(super) fn refusal_text(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "user_namespace" => "程序不在主機原始使用者命名空間，或無法確認使用者與群組對應。",
        "container_environment" => "偵測到容器環境，或無法確認原生主機環境。",
        "key_state_poisoned" => "金鑰操作發生異常，已清除金鑰；請重新啟動服務。",
        "reattest_unavailable" => "無法啟動定期保護檢查，已停用個人帳號；請重新啟動服務。",
        "mode" => "啟動模式不符，無法接收獨立金鑰。",
        "not_systemd_child" => "DuDuClaw 不是由系統服務管理員直接啟動。",
        "cgroup_v1" => "此模式需要單一 cgroup v2 階層，不支援舊版或混合階層。",
        "cgroup_unit" => "DuDuClaw 不在指定的系統服務群組內，或無法確認群組。",
        "exe_stat" => "無法確認正在執行的 DuDuClaw 程式檔案。",
        "exe_type" => "DuDuClaw 程式必須是一般檔案。",
        "exe_owner" => "DuDuClaw 程式不屬於 root，無法確認保護。",
        "exe_writable" => "其他帳號能修改 DuDuClaw 程式，無法接收金鑰。",
        "exe_ancestor_type" => "DuDuClaw 安裝路徑含不受支援的目錄連結。",
        "exe_ancestor_owner" => "DuDuClaw 安裝路徑的目錄不全屬於 root。",
        "exe_ancestor_writable" => "其他帳號能修改 DuDuClaw 安裝路徑的目錄。",
        "supplementary_groups" => "DuDuClaw 仍有額外群組權限，或無法確認已清除。",
        "engine_socket_reachable" => "偵測到可存取的容器引擎控制介面，或無法確認存取已被拒絕。同機其他程式（常見於開發者啟動的容器）可能暴露此介面，個人帳號已暫停。預設會在介面移除、環境檢查通過後自動重新取得金鑰；自動恢復關閉、次數用盡或無法保存狀態時會繼續暫停。建議將開發容器移到其他主機；若為舊版 containerd shim，請升級 containerd。",
        "engine_socket" => "容器裡看得到引擎控制介面，AI 員工可能藉此讀到金鑰。",
        "mount_scan_incomplete" => "無法完整檢查容器的引擎控制介面掛載。",
        "inactive" => "程序保護沒有啟用，無法接收獨立金鑰。",
        "fd_contract" => "啟動步驟沒有使用指定的金鑰交接方式。",
        "fd_stat" => "無法確認啟動步驟交來的金鑰檔案。",
        "fd_type" => "啟動步驟交來的金鑰不是一般檔案。",
        "fd_owner" => "金鑰檔案不屬於 root，無法確認隔離。",
        "fd_mode" => "金鑰檔案的權限不是只有 root 可讀。",
        "fd_links" => "金鑰檔案有額外連結，無法確認隔離。",
        "fd_size" => "金鑰檔案長度不正確。",
        "fd_read" => "無法完整接收啟動步驟交來的金鑰。",
        "not_pid_one" => "DuDuClaw 不是容器的主程序，停止後仍可能留下其他程序。",
        "user_ids" => "DuDuClaw 的使用者權限沒有完整降為一般帳號。",
        "group_ids" => "DuDuClaw 的群組權限沒有完整降為一般群組。",
        "proc_status" => "無法確認程序的安全設定。",
        "no_new_privs" => "程序未禁止取得額外權限。",
        "cap_inh" => "程序仍保有可繼承的額外權限。",
        "cap_prm" => "程序仍保有可啟用的額外權限。",
        "cap_eff" => "程序仍在使用額外權限。",
        "cap_bnd" => "程序仍可取得額外權限。",
        "cap_amb" => "程序仍會傳遞額外權限給新程序。",
        "dumpable" => "程序未能禁止其他程序讀取 DuDuClaw 的記憶體。",
        "key_path_accessible" => "無法確認一般帳號已被禁止直接開啟根金鑰。",
        "docker_socket" => "容器裡看得到 Docker 控制介面，或無法確認其不存在，AI 員工可能藉此讀到金鑰。",
        "docker_host" => "程序設定了 Docker 控制介面，AI 員工可能藉此讀到金鑰。",
        _ => return None,
    })
}

fn with_extra(base: &str, extra: &str) -> String {
    if extra.is_empty() {
        base.to_string()
    } else {
        format!("{base}（{}）", duduclaw_core::truncate_chars(extra, 120))
    }
}

/// Persisted diagnostics are history, never evidence that a different process
/// (for example `doctor`) currently holds or has revoked the gateway's key.
pub(super) fn recovery_detail(
    status: &duduclaw_core::root_key::recovery::RecoveryStatus,
    currently_refused: bool,
) -> String {
    let mut text = String::new();
    if let Some(revocation) = &status.latest_revocation {
        text.push_str(&format!(" 最近一次金鑰拒絕或撤銷（UTC {}）：{}",
            duduclaw_core::root_key::recovery::sanitize_diagnostic(&revocation.timestamp), refusal_text(revocation.reason.as_str()).unwrap_or("保護檢查未通過。")));
        if let Some(detail) = &revocation.detail {
            text.push_str(&format!(" 偵測詳情：{}。", duduclaw_core::root_key::recovery::sanitize_diagnostic(detail)));
        }
    }
    if currently_refused {
        text.push_str(match status.recovery.as_str() {
            "disabled" => " 自動恢復已關閉；排除原因後，請由管理者重新啟動受保護服務。",
            "waiting" | "monitoring" => " 每分鐘重新檢查，連續三次通過後會重新啟動服務，重新執行完整金鑰交接檢查。",
            "budget_exhausted" => " 已用完每小時四次自動重啟的建議額度；系統服務管理員另限制每小時的啟動總數，一般故障重啟也會計入。達到系統限制後，請由管理者確認環境並重新啟動。",
            "restart_requested" => " 已要求正常關機；服務管理員將重新啟動服務並重新執行完整金鑰交接檢查。",
            "state_unavailable" => " 無法讀取或保存恢復狀態，因此不會自動重啟。請檢查服務資料目錄的權限與可用空間。",
            "non_transient" => " 此原因不會自動恢復；請修正保護設定後，由管理者重新啟動受保護服務。",
            _ => " 無法確認自動恢復狀態，請由管理者檢查受保護服務。",
        });
    } else if status.latest_revocation.is_some() {
        text.push_str(" 以上是歷史記錄，不代表服務目前仍被拒絕；請以儀表板的目前保護狀態為準。");
    }
    text
}
