//! v1.80 protected-TEAM limitation. This policy is never a local-mode gate.
//! Keep the full tool inventory: defer only the three unfinished mutations.
pub const DEFERRED_TOOLS: [&str; 3] = ["fork_run", "merge_or_select", "terminate_branch"];
pub const READ_TOOLS: [&str; 3] = ["inspect_branches", "diff_branches", "fork_cost"];
pub const HELPER_TOOLS: [&str; 5] = ["execute_program", "office_script", "code_map",
    "browser_record_start", "browser_record_stop"];

pub fn deferred(name: &str) -> bool { DEFERRED_TOOLS.contains(&name) }

pub fn message(language: Option<&str>) -> &'static str {
    match language {
        Some("en" | "en-US" | "en-GB") => "Branch trials in protected mode will be available in the next release.",
        Some("ja" | "ja-JP") => "保護モードでの分岐試行は次のバージョンで利用可能になります。",
        _ => "受保護模式下的分支試算將於下一版開放。",
    }
}

/// Source inventory readiness is separate from native/provider acceptance.
/// Missing signed helpers or read services must still block A readiness.
pub fn remaining_producers_ready(mut helper: impl FnMut(&str) -> bool,
    mut reader: impl FnMut(&str) -> bool) -> bool {
    HELPER_TOOLS.into_iter().all(&mut helper) && READ_TOOLS.into_iter().all(&mut reader)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_three_exact_fork_mutations_are_deferred() {
        for name in DEFERRED_TOOLS { assert!(deferred(name)); }
        for name in READ_TOOLS.into_iter().chain(HELPER_TOOLS).chain(["fork_run_extra", "fork", "create_task"]) {
            assert!(!deferred(name), "{name}");
        }
    }
    #[test]
    fn remaining_readiness_requires_every_real_service_but_no_deferred_producer() {
        assert!(remaining_producers_ready(|name| HELPER_TOOLS.contains(&name), |name| READ_TOOLS.contains(&name)));
        for missing in HELPER_TOOLS.into_iter().chain(READ_TOOLS) {
            assert!(!remaining_producers_ready(|name| name != missing, |name| name != missing), "{missing}");
        }
        assert!(!remaining_producers_ready(|_| false, |_| true));
        assert!(!remaining_producers_ready(|_| true, |_| false));
    }
    #[test]
    fn deferred_message_has_three_locales_and_safe_default() {
        assert_eq!(message(None), message(Some("zh-TW")));
        assert_eq!(message(Some("unknown")), message(None));
        assert_eq!(message(Some("en")), message(Some("en-US")));
        assert_eq!(message(Some("ja")), message(Some("ja-JP")));
        assert!(message(None).contains("下一版"));
        assert!(message(Some("en")).contains("next release"));
        assert!(message(Some("ja")).contains("次のバージョン"));
    }
}
