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
            summary("檔案（這個平台不支援系統金鑰圈）"),
            "這個平台（例如 Linux 或 Docker）還沒有支援的系統金鑰圈，AI 員工不能使用個人的 Google 帳號。".into(),
        ),
        ProtectionState::Unavailable => (
            summary("系統金鑰圈無法使用"),
            with_extra("暫時讀不到系統金鑰圈。個人帳號暫停使用；其他功能不受影響。", extra),
        ),
    }
}

fn ready_detail(level: ProtectionLevel, backend: BackendKind) -> &'static str {
    match (level, backend) {
        (ProtectionLevel::AppBound, _) => "個人帳號的憑證用一把只存在這台 Mac 金鑰圈的金鑰加密，其他程式要讀時 macOS 會擋下。能執行指令的 AI 員工仍可能換掉這把金鑰；gateway 執行期間換掉、或舊的封存資料對不上時，DuDuClaw 會停用個人帳號。",
        (ProtectionLevel::UserStore, BackendKind::MacosKeychain) => "個人帳號的憑證用一把只存在這台 Mac 金鑰圈的金鑰加密，一般程式讀不到。但這個 DuDuClaw 執行檔沒有開啟 hardened runtime，能執行指令的 AI 員工可以把程式碼注入 DuDuClaw 後讀到這把金鑰。",
        (ProtectionLevel::UserStore, _) => "個人帳號的憑證用一把存在 Windows 認證管理員的金鑰加密，不會出現在檔案或備份裡。同一個 Windows 帳號下的程式（包括能執行指令的 AI 員工）仍讀得到。",
        (ProtectionLevel::File, _) => "",
    }
}

fn with_extra(base: &str, extra: &str) -> String {
    if extra.is_empty() {
        base.to_string()
    } else {
        format!("{base}（{}）", duduclaw_core::truncate_chars(extra, 120))
    }
}
