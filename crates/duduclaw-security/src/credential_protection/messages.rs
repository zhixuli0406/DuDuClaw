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
            ProtectionState::KeyChanged => Some(("保護金鑰被更換", "加密保存的保護金鑰無法驗證，可能被改動；金鑰磁碟區遺失或被更換也會造成這個狀況。個人帳號已停用。請先檢查容器和金鑰磁碟區，再重新啟用並請大家重新連結。".to_string())),
            ProtectionState::Unavailable => Some(("獨立金鑰無法使用", with_extra("無法使用容器的獨立金鑰。個人帳號暫停使用；其他功能不受影響。", refusal_text(extra).unwrap_or(extra)))),
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
            "一般 Docker 或 Linux 安裝沒有 AI 員工讀不到的金鑰存放處，因此個人帳號維持停用。Docker 可使用部署指南的獨立金鑰啟動模式；原生 Linux 尚未支援。在容器終端機查詢也會顯示此狀態，請以儀表板顯示的實際狀態為準。".into(),
        ),
        ProtectionState::Unavailable => (
            summary("系統金鑰圈無法使用"),
            with_extra("暫時讀不到系統金鑰圈。個人帳號暫停使用；其他功能不受影響。", extra),
        ),
    }
}

fn ready_detail(level: ProtectionLevel, backend: BackendKind) -> &'static str {
    match (level, backend) {
        (ProtectionLevel::ProcessIsolated, _) => "個人帳號的憑證以獨立金鑰保護，只有容器的啟動步驟（root）能讀取根金鑰，並在啟動時交給 DuDuClaw 主程式一次。能執行指令的 AI 員工無法讀取或替換根金鑰，但仍能在允許的範圍內要求 DuDuClaw 代用 Google、讀取該回合取得的存取權杖，也能讓 DuDuClaw 停止運作。控制 Docker 主機或金鑰磁碟區的人能讀取根金鑰。",
        (ProtectionLevel::AppBound, _) => "個人帳號的憑證用一把只存在這台 Mac 金鑰圈的金鑰加密，其他程式要讀時 macOS 會擋下。能執行指令的 AI 員工仍可能換掉這把金鑰；gateway 執行期間換掉、或舊的封存資料對不上時，DuDuClaw 會停用個人帳號。",
        (ProtectionLevel::UserStore, BackendKind::MacosKeychain) => "個人帳號的憑證用一把只存在這台 Mac 金鑰圈的金鑰加密，一般程式讀不到。但這個 DuDuClaw 執行檔沒有開啟 hardened runtime，能執行指令的 AI 員工可以把程式碼注入 DuDuClaw 後讀到這把金鑰。",
        (ProtectionLevel::UserStore, _) => "個人帳號的憑證用一把存在 Windows 認證管理員的金鑰加密，不會出現在檔案或備份裡。同一個 Windows 帳號下的程式（包括能執行指令的 AI 員工）仍讀得到。",
        (ProtectionLevel::File, _) => "",
    }
}

/// Closed attestation diagnostics; do not show implementation codes as prose.
pub(super) fn refusal_text(reason: &str) -> Option<&'static str> {
    Some(match reason {
        "inactive" => "容器的程序保護沒有啟用，無法接收獨立金鑰。",
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
        "proc_status" => "無法確認容器程序的安全設定。",
        "no_new_privs" => "容器未禁止程序取得額外權限。",
        "cap_inh" => "容器程序仍保有可繼承的額外權限。",
        "cap_prm" => "容器程序仍保有可啟用的額外權限。",
        "cap_eff" => "容器程序仍在使用額外權限。",
        "cap_bnd" => "容器程序仍可取得額外權限。",
        "cap_amb" => "容器程序仍會傳遞額外權限給新程序。",
        "dumpable" => "容器未能禁止其他程序讀取 DuDuClaw 的記憶體。",
        "key_path_accessible" => "無法確認一般帳號已被禁止直接開啟根金鑰。",
        "docker_socket" => "容器裡看得到 Docker 控制介面，或無法確認其不存在，AI 員工可能藉此讀到金鑰。",
        "docker_host" => "容器設定了 Docker 控制介面，AI 員工可能藉此讀到金鑰。",
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
