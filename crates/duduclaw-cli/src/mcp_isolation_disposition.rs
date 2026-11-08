//! Isolation changes execution location, not employee permissions.
//! Operator-only tools are refused by the existing gates, never this table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal { HostAuthority, ForkNextRelease, ProposalRequired }
impl Refusal {
    pub fn message(self) -> &'static str {
        self.message_for_language(None)
    }
    pub fn message_for_language(self, language: Option<&str>) -> &'static str {
        match self {
            Self::HostAuthority => "隔離執行沒有主機桌面、裝置或系統服務的控制權。",
            Self::ForkNextRelease => duduclaw_mcp_boundary::fork_policy::message(language),
            Self::ProposalRequired => PROPOSAL_REQUIRED,
        }
    }
}

/// Native Linux protected mode (v1.81, SR2): a protected employee exists only
/// through an approved Employee proposal and its root allocation, so an
/// employee can never create another one itself.
pub const PROPOSAL_REQUIRED: &str = "受保護模式下 AI 員工不能自行建立新的 AI 員工。請改用員工配置提案：\
    管理員在儀表板提出提案並通過密碼與雙因素驗證後，在核准卡片上確認；同仁可在自己的帳號頁申請個人專屬員工，由管理員核准。";

/// Exact host-authority exceptions and the explicit v1.80 fork deferral.
/// Gateway-owned computer containers retain their original session handlers.
pub fn intentional_refusal(name: &str) -> Option<Refusal> {
    use Refusal::*;
    if duduclaw_mcp_boundary::fork_policy::deferred(name) { return Some(ForkNextRelease); }
    Some(match name {
        "os_notify" | "os_watch_status" | "os_open" | "os_frontmost" | "os_spotlight_search" |
        "os_calendar_today" | "os_device_status" | "os_system_status" | "os_check_update" |
        "os_backup_list" | "os_network_info" | "os_wifi_status" | "os_wifi_scan" | "os_wifi_connect" |
        "os_apply_update" | "os_boot_assessment" | "os_update_rollback" | "os_backup_create" |
        "os_power" | "os_factory_reset" | "os_doctor_repair" | "os_display_get" | "os_display_set" |
        "os_audio_get" | "os_audio_set" | "desktop_record_start" | "desktop_record_stop" => HostAuthority,
        "codrive_run" | "codrive_status" | "model_load" | "model_unload" |
        "model_download" | "llamafile_start" | "llamafile_stop" => HostAuthority,
        // Employee creation goes through the Employee proposal path only.
        "create_agent" => ProposalRequired,
        _ => return None,
    })
}
