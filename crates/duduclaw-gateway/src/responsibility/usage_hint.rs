//! M3-2: runtimes whose token usage a responsibility cannot rely on.
//!
//! Spend is counted from `token_usage` rows. A runtime that writes no usage
//! for a round (or a row with zero tokens) leaves that round unmeasured, and
//! an unmeasured round that ran is charged the full per-run cap
//! (`cost::running_spend` / `settle_charge`). With such a runtime a run
//! usually gets one round: the next round's cost check finds the cap used and
//! sends the run to a human. The same applies when the acceptance judge runs
//! on such a runtime, because its call is charged to the run too.
//!
//! The hint is shown when a responsibility is created (RPC and command line)
//! and by `duduclaw doctor`. It is advisory: nothing is refused.

use std::path::Path;

use duduclaw_core::types::RuntimeType;

/// How a runtime reports usage, as far as responsibility spend is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageReporting {
    /// Usage comes from the provider (the normal case).
    Reported,
    /// The runtime may return no usage at all for a round; such a round is
    /// charged the full per-run cap.
    MayBeMissing,
    /// The runtime never reports usage; the gateway estimates it from text
    /// length.
    Estimated,
}

/// Classification per runtime. Antigravity (no `usage` block in some agy
/// results) and the Gemini CLI (no `stats` block in some results) can leave
/// a round with zero tokens; Grok reports nothing and is estimated.
pub fn usage_reporting(rt: RuntimeType) -> UsageReporting {
    match rt {
        RuntimeType::Antigravity | RuntimeType::Gemini => UsageReporting::MayBeMissing,
        RuntimeType::Grok => UsageReporting::Estimated,
        _ => UsageReporting::Reported,
    }
}

fn runtime_label(rt: RuntimeType) -> &'static str {
    match rt {
        RuntimeType::Antigravity => "Antigravity",
        RuntimeType::Gemini => "Gemini CLI",
        RuntimeType::Grok => "Grok",
        RuntimeType::Codex => "Codex",
        RuntimeType::OpenAiCompat => "OpenAI 相容服務",
        _ => "Claude",
    }
}

/// The two runtimes a run's spend depends on: the employee's own and the
/// acceptance judge's (`[dispatch] judge_provider`, else the utility model
/// the employee resolves to).
pub fn run_runtimes(home: &Path, owner: &str) -> (RuntimeType, RuntimeType) {
    let agent_dir = home.join("agents").join(owner);
    let employee = crate::runtime_config::agent_runtime_provider(&agent_dir);
    let judge = crate::judge_mode::judge_model_hint_from_home(Some(home))
        .and_then(|h| h.provider)
        .unwrap_or_else(|| crate::runtime_config::resolve_utility(home, Some(&agent_dir)).provider);
    (employee, judge)
}

/// Pure half of [`usage_warnings`]: the sentences for one pair of runtimes.
pub fn warnings_for(employee: RuntimeType, judge: RuntimeType) -> Vec<String> {
    let mut out = Vec::new();
    match usage_reporting(employee) {
        UsageReporting::MayBeMissing => out.push(format!(
            "這位 AI 員工使用 {}，它不一定回報用量。沒有回報用量的那一輪會以單次花費上限全額計算，所以一次執行通常只跑得了一輪，被駁回後就會轉給人處理。",
            runtime_label(employee)
        )),
        UsageReporting::Estimated => out.push(format!(
            "這位 AI 員工使用 {}，它不回報用量，花費是依送出與收到的文字長度估算的。",
            runtime_label(employee)
        )),
        UsageReporting::Reported => {}
    }
    match usage_reporting(judge) {
        UsageReporting::MayBeMissing => out.push(format!(
            "驗收判官使用 {}，它不一定回報用量。判官那次呼叫沒有回報時，這次執行同樣以單次花費上限全額計算。",
            runtime_label(judge)
        )),
        // L4-8: an estimated judge is named too.
        UsageReporting::Estimated => out.push(format!(
            "驗收判官使用 {}，它不回報用量，判官那次呼叫的花費是依文字長度估算的。",
            runtime_label(judge)
        )),
        UsageReporting::Reported => {}
    }
    out
}

/// Advisory sentences for a responsibility owned by `owner` (empty when the
/// runtimes report usage normally).
pub fn usage_warnings(home: &Path, owner: &str) -> Vec<String> {
    let (employee, judge) = run_runtimes(home, owner);
    warnings_for(employee, judge)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtimes_that_report_usage_need_no_hint() {
        assert!(warnings_for(RuntimeType::Claude, RuntimeType::Claude).is_empty());
        assert!(warnings_for(RuntimeType::Codex, RuntimeType::OpenAiCompat).is_empty());
    }

    #[test]
    fn a_missing_usage_runtime_is_named_for_the_employee_and_the_judge() {
        let w = warnings_for(RuntimeType::Antigravity, RuntimeType::Claude);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("Antigravity") && w[0].contains("全額"),
            "{w:?}"
        );
        let w = warnings_for(RuntimeType::Claude, RuntimeType::Gemini);
        assert_eq!(w.len(), 1);
        assert!(
            w[0].contains("驗收判官") && w[0].contains("Gemini"),
            "{w:?}"
        );
        assert_eq!(
            warnings_for(RuntimeType::Antigravity, RuntimeType::Antigravity).len(),
            2
        );
    }

    #[test]
    fn a_grok_judge_is_named_as_estimated() {
        let w = warnings_for(RuntimeType::Claude, RuntimeType::Grok);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("驗收判官") && w[0].contains("估算"), "{w:?}");
    }

    #[test]
    fn grok_is_named_as_estimated() {
        let w = warnings_for(RuntimeType::Grok, RuntimeType::Claude);
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("估算"), "{w:?}");
    }

    #[test]
    fn the_owner_and_judge_runtimes_are_read_from_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let agent = dir.path().join("agents").join("worker");
        std::fs::create_dir_all(&agent).unwrap();
        std::fs::write(
            agent.join("agent.toml"),
            "[agent]\nname = \"worker\"\n\n[runtime]\nprovider = \"antigravity\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[dispatch]\njudge_provider = \"gemini\"\n",
        )
        .unwrap();
        let (employee, judge) = run_runtimes(dir.path(), "worker");
        assert_eq!(employee, RuntimeType::Antigravity);
        assert_eq!(judge, RuntimeType::Gemini);
        assert_eq!(usage_warnings(dir.path(), "worker").len(), 2);
    }
}
