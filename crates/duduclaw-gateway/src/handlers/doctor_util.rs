//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Check if an API key is available (env var, `config.toml [api]`, or a
/// non-empty `accounts` array). Pure function of `home_dir` — extracted from
/// `MethodHandler::has_api_key` (now a thin delegate to this) so the O-0
/// agent-facing `os_doctor_repair` MCP tool (`duduclaw-cli::mcp`) can reuse
/// the EXACT same three-source check instead of re-deriving it, avoiding the
/// two-implementation drift the O-0 design explicitly calls out.
pub async fn has_api_key_configured(home_dir: &Path) -> bool {
    // 1. Check environment variable
    if std::env::var("ANTHROPIC_API_KEY").is_ok_and(|k| !k.is_empty()) {
        return true;
    }
    // 2. Check config.toml [api] section
    let table = match tokio::fs::read_to_string(home_dir.join("config.toml")).await {
        Ok(content) => content.parse::<toml::Table>().unwrap_or_default(),
        Err(_) => toml::Table::new(),
    };
    if let Some(api) = table.get("api").and_then(|v| v.as_table())
        && api
            .get("anthropic_api_key")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty())
    {
        return true;
    }
    // 3. Check accounts in config.toml
    if let Some(accounts) = table.get("accounts")
        && let Some(arr) = accounts.as_array()
    {
        return !arr.is_empty();
    }
    false
}

/// Shared status and operator guidance for both doctor surfaces. Raw probe
/// output stays internal; only the fixed zh-TW guidance reaches the UI.
pub async fn codex_sandbox_status_and_message() -> (&'static str, &'static str) {
    let binary = duduclaw_core::which_codex().unwrap_or_else(|| "codex".into());
    let result = crate::runtime::codex_sandbox::probe(&binary).await;
    (if result.blocked() { "warn" } else { "pass" }, result.message(crate::updater::is_containerized()))
}

/// Container-only guidance, shared by dashboard and operator CLI doctor.
/// The standalone CLI probes itself and can therefore warn even if a running
/// gateway is already hardened; only the gateway RPC proves its live state.
pub fn container_process_isolation_status_and_message() -> Option<(&'static str, &'static str)> {
    if !duduclaw_core::ns_guard::is_containerized() {
        return None;
    }
    let scope = std::fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope").ok();
    Some(container_process_isolation_result(
        duduclaw_core::ns_guard::gateway_process_is_non_dumpable(), scope.as_deref(),
    ))
}

fn container_process_isolation_result(non_dumpable: bool, scope: Option<&str>) -> (&'static str, &'static str) {
    if !scope.and_then(|v| v.trim().parse::<u32>().ok()).is_some_and(|v| v >= 1) {
        return ("warn", "這台主機沒有開啟 Yama，員工之間的行程保護較弱；單一租戶可接受，多租戶請改用開啟 Yama 的 Linux 主機。");
    }
    if !non_dumpable {
        return ("warn", "目前行程尚未啟用 non-dumpable 保護；請重新啟動 gateway，並從儀表板執行健檢以確認 gateway 的保護狀態。");
    }
    ("pass", "Gateway 已啟用 non-dumpable 保護，主機 Yama 行程隔離已開啟。")
}

#[cfg(test)]
mod container_process_isolation_tests {
    use super::*;

    #[test]
    fn container_process_isolation_requires_both_protections() {
        for scope in [None, Some("0"), Some("bad"), Some("")] {
            for hardened in [false, true] {
                let (status, message) = container_process_isolation_result(hardened, scope);
                assert_eq!(status, "warn");
                assert!(message.contains("這台主機沒有開啟 Yama"));
            }
        }
        for scope in ["1", "2", "3\n"] {
            assert_eq!(container_process_isolation_result(true, Some(scope)).0, "pass");
            assert_eq!(container_process_isolation_result(false, Some(scope)).0, "warn");
        }
    }
}

/// Repair-hint text for one failing/warning `system.doctor` check name.
/// Extracted from `MethodHandler::handle_system_doctor_repair` (now a thin
/// delegate) so the O-0 `os_doctor_repair` MCP tool (`duduclaw-cli::mcp`)
/// shares the exact same hint copy instead of re-deriving it.
pub fn doctor_repair_hint(check_name: &str) -> &'static str {
    match check_name {
        "container_process_isolation" => "請由管理者確認 gateway 的 non-dumpable 保護；多租戶請使用開啟 Yama（ptrace_scope >= 1）的 Linux 主機。",
        "codex_employee_shell" => "請依此檢查項目的訊息，由管理者確認主機的 user namespace 與容器設定。",
        "agents" => "Run 'duduclaw agent create <name>' to create your first agent.",
        "api_key" => "Set ANTHROPIC_API_KEY environment variable with a valid key.",
        "config_file" => "Run 'duduclaw init' to create a default config.toml.",
        _ => "Check the documentation for repair instructions.",
    }
}
