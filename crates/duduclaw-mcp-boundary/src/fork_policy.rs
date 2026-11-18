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
/// R8n-1: a helper tool is required only when this host `offered` it (its
/// family is in the signed policy); an offered helper without its producer
/// and a missing read service still block A readiness. With no helper
/// offered, only the read services remain.
pub fn remaining_producers_ready(mut offered: impl FnMut(&str) -> bool, mut helper: impl FnMut(&str) -> bool,
    mut reader: impl FnMut(&str) -> bool) -> bool {
    HELPER_TOOLS.into_iter().all(|name| !offered(name) || helper(name)) && READ_TOOLS.into_iter().all(&mut reader)
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
        let all = |_: &str| true;
        assert!(remaining_producers_ready(all, |name| HELPER_TOOLS.contains(&name), |name| READ_TOOLS.contains(&name)));
        for missing in HELPER_TOOLS.into_iter().chain(READ_TOOLS) {
            assert!(!remaining_producers_ready(all, |name| name != missing, |name| name != missing), "{missing}");
        }
        assert!(!remaining_producers_ready(all, |_| false, |_| true));
        assert!(!remaining_producers_ready(all, |_| true, |_| false));
    }

    /// R8n-1: only offered helpers need a producer; the read services are
    /// always required, whatever subset is offered (including none).
    #[test]
    fn remaining_readiness_requires_exactly_the_offered_helpers() {
        for mask in 0u32..(1 << HELPER_TOOLS.len()) {
            let offered = |name: &str| HELPER_TOOLS.iter().position(|tool| *tool == name)
                .is_some_and(|index| mask & (1 << index) != 0);
            // Producers exactly for the offered ones: ready.
            assert!(remaining_producers_ready(offered, offered, |_| true), "{mask:b}");
            // No helper producer at all: ready only when nothing is offered.
            assert_eq!(remaining_producers_ready(offered, |_| false, |_| true), mask == 0, "{mask:b}");
            // A missing read service always blocks.
            assert!(!remaining_producers_ready(offered, offered, |_| false), "{mask:b}");
        }
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
