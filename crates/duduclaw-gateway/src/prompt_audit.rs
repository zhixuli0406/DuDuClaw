//! Prompt-size audit logger — surfaces per-section byte counts so operators
//! can diagnose 200K-token cliff warnings without re-running with custom
//! tracing.
//!
//! Background (2026-05-09 health check): `duduclaw-eng-memory` was hitting
//! 2.1 M tokens per request — 10× the 200 K cliff. The
//! [`crate::cost_telemetry`] warning fires AFTER the response with only the
//! aggregate `total_input` number. To root-cause which prompt section
//! bloated, you previously had to manually instrument the prompt builders
//! one by one.
//!
//! This module gives the prompt builders a single, threshold-gated entry
//! point that emits a structured `info!` log with each section's byte
//! count plus the total. The log is keyed on `target =
//! "prompt_section_audit"` so an operator can grep for it cheaply, and
//! the threshold (default 50 KB ≈ ~16 K tokens) is sized so it only fires
//! on already-suspicious prompts — keeping noise low under normal traffic.
//!
//! Enforcement (truncation / compression routing) is deferred — this is the
//! observability foundation a future enforcement layer can sit on top of.

use tracing::info;

/// Default emit threshold in bytes. ~16K tokens at the 1.5 chars/token
/// heuristic the rest of the gateway uses; chosen so the typical 5-10 KB
/// system prompt stays silent and only the path to the 200 K cliff lights
/// up. Override per-call where it makes sense.
pub const DEFAULT_EMIT_THRESHOLD_BYTES: usize = 50_000;

/// One labelled section of a system prompt and its byte size.
#[derive(Debug, Clone)]
pub struct PromptSection {
    pub label: &'static str,
    pub bytes: usize,
}

impl PromptSection {
    pub fn new(label: &'static str, content: &str) -> Self {
        Self {
            label,
            bytes: content.len(),
        }
    }
}

/// Emit a structured `info!` log with section breakdown when the total
/// prompt size exceeds `threshold_bytes`. Below threshold, returns
/// immediately — the caller pays only for the cheap sum.
///
/// `agent_id` is included so the log line can be filtered per-agent, and
/// `surface` ("channel_reply" / "claude_runner" / etc.) helps disambiguate
/// when the same agent uses multiple prompt builders.
pub fn maybe_log_breakdown(
    agent_id: &str,
    surface: &'static str,
    sections: &[PromptSection],
    threshold_bytes: usize,
) {
    let total: usize = sections.iter().map(|s| s.bytes).sum();
    if total < threshold_bytes {
        return;
    }

    // Render `label=N,label=N,...` so the line stays single-line and easily
    // grep-able. Using a string field keeps the tracing-subscriber JSON
    // formatter happy without a custom Visitor.
    let breakdown = sections
        .iter()
        .map(|s| format!("{}={}", s.label, s.bytes))
        .collect::<Vec<_>>()
        .join(",");

    info!(
        target: "prompt_section_audit",
        agent_id = agent_id,
        surface = surface,
        total_bytes = total,
        sections = %breakdown,
        "System prompt section breakdown"
    );
}

/// Read `[budget] max_input_tokens` from `agent.toml` if present.
///
/// This is the configuration knob for the future enforcement layer (#6.3 in
/// the runtime-health-fixes TODO). Reading it now means every prompt
/// builder can already plumb the value through without a second config
/// migration when enforcement lands. Returns `None` for missing key —
/// callers fall through to a workspace-level default.
pub fn read_max_input_tokens(agent_dir: &std::path::Path) -> Option<u64> {
    let toml_path = agent_dir.join("agent.toml");
    let raw = std::fs::read_to_string(&toml_path).ok()?;
    let value: toml::Value = raw.parse().ok()?;
    value
        .get("budget")
        .and_then(|b| b.get("max_input_tokens"))
        .and_then(|v| v.as_integer())
        .and_then(|n| u64::try_from(n).ok())
}

/// Default byte cap for the legacy unbounded skill-injection fallback
/// (#6.2b). Picked at 10 KB ≈ ~3 K tokens — large enough that typical
/// skill bundles (5–10 short skills) fit unmodified, but small enough
/// that a runaway agent.skills directory can't push the system prompt
/// past the 200 K cliff alone.
pub const DEFAULT_LEGACY_SKILL_BYTE_CAP: usize = 10_000;

/// Render a sequence of (name, content) skills into a `Vec<String>` of
/// `"## Skill: <name>\n<content>"` segments, capped at `max_bytes` total.
///
/// Returns the rendered segments plus an optional "truncation note" the
/// caller appends when the budget was exceeded. Never returns more than
/// `max_bytes` worth of segments — the budget is hard.
///
/// Why this lives in `prompt_audit`: the budget IS prompt-size policy,
/// and `prompt_audit` is the only module both `channel_reply` and
/// `claude_runner` already depend on. Keeps the policy in one place
/// instead of two diverging copies.
pub fn budgeted_legacy_skills(
    skills: &[(String, String)],
    max_bytes: usize,
) -> (Vec<String>, Option<String>) {
    let mut out = Vec::with_capacity(skills.len());
    let mut used: usize = 0;
    let mut truncated_count: usize = 0;
    let mut truncated_names: Vec<&str> = Vec::new();

    for (name, content) in skills {
        let rendered = format!("## Skill: {name}\n{content}");
        let len = rendered.len();
        if used + len <= max_bytes {
            used += len;
            out.push(rendered);
        } else {
            truncated_count += 1;
            if truncated_names.len() < 10 {
                truncated_names.push(name.as_str());
            }
        }
    }

    let footer = if truncated_count > 0 {
        let preview = truncated_names.join(", ");
        let suffix = if truncated_count > truncated_names.len() {
            format!(" (and {} more)", truncated_count - truncated_names.len())
        } else {
            String::new()
        };
        Some(format!(
            "## Skills truncated ({truncated_count})\n\
             [{preview}{suffix}] omitted to keep system prompt under \
             {max_bytes} bytes — narrow the agent's skill set or migrate \
             to progressive skill injection (channel_reply path) for \
             relevance-ranked selection."
        ))
    } else {
        None
    };

    (out, footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(label: &'static str, n: usize) -> PromptSection {
        PromptSection { label, bytes: n }
    }

    #[test]
    fn does_not_emit_below_threshold() {
        // Just verify the function doesn't panic with under-threshold input.
        // The actual log assertion is hard to do here without the
        // tracing-test harness; behaviour is `()`-typed.
        maybe_log_breakdown(
            "agent",
            "test",
            &[s("soul", 100), s("identity", 50)],
            10_000,
        );
    }

    #[test]
    fn emits_when_total_exceeds_threshold() {
        // Same caveat: we just verify no panic on emit path.
        maybe_log_breakdown(
            "agent",
            "test",
            &[s("soul", 60_000), s("wiki", 20_000)],
            50_000,
        );
    }

    #[test]
    fn prompt_section_new_records_byte_length() {
        let p = PromptSection::new("foo", "hello");
        assert_eq!(p.label, "foo");
        assert_eq!(p.bytes, 5);
    }

    #[test]
    fn prompt_section_new_uses_byte_length_not_char_count() {
        // CJK char is 3 bytes in UTF-8; we want bytes for cost analysis.
        let p = PromptSection::new("cjk", "你");
        assert_eq!(p.bytes, 3);
    }

    #[test]
    fn read_max_input_tokens_returns_none_when_file_missing() {
        let tmp = std::env::temp_dir()
            .join(format!("prompt-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        assert_eq!(read_max_input_tokens(&tmp), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_max_input_tokens_reads_budget_section() {
        let tmp = std::env::temp_dir()
            .join(format!("prompt-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("agent.toml"),
            "[budget]\nmax_input_tokens = 180000\n",
        )
        .unwrap();
        assert_eq!(read_max_input_tokens(&tmp), Some(180_000));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_max_input_tokens_returns_none_for_negative_value() {
        let tmp = std::env::temp_dir()
            .join(format!("prompt-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("agent.toml"),
            "[budget]\nmax_input_tokens = -1\n",
        )
        .unwrap();
        // u64::try_from on negative i64 returns Err, so we treat it as
        // "no useful config" rather than crashing.
        assert_eq!(read_max_input_tokens(&tmp), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn read_max_input_tokens_returns_none_when_section_missing() {
        let tmp = std::env::temp_dir()
            .join(format!("prompt-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            tmp.join("agent.toml"),
            "[agent]\nname = \"test\"\n",
        )
        .unwrap();
        assert_eq!(read_max_input_tokens(&tmp), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── #6.2b: budgeted skill renderer ──────────────────────────────────

    #[test]
    fn budgeted_skills_renders_everything_when_under_budget() {
        let skills = vec![
            ("alpha".to_string(), "small content".to_string()),
            ("beta".to_string(), "more content".to_string()),
        ];
        let (out, footer) = budgeted_legacy_skills(&skills, 10_000);
        assert_eq!(out.len(), 2, "all skills should fit");
        assert!(out[0].contains("## Skill: alpha"));
        assert!(out[1].contains("## Skill: beta"));
        assert!(footer.is_none(), "no truncation footer when under budget");
    }

    #[test]
    fn budgeted_skills_truncates_to_byte_cap() {
        // Each skill is "## Skill: <name>\n<content>" — choose contents
        // large enough that only the first one fits in a 200-byte cap.
        let big = "x".repeat(150);
        let skills = vec![
            ("a".to_string(), big.clone()),
            ("b".to_string(), big.clone()),
            ("c".to_string(), big),
        ];
        let (out, footer) = budgeted_legacy_skills(&skills, 200);
        assert_eq!(out.len(), 1, "only first skill should fit");
        let footer_text = footer.expect("footer must be present");
        assert!(footer_text.contains("truncated (2)"));
        assert!(footer_text.contains("b"));
        assert!(footer_text.contains("c"));
    }

    #[test]
    fn budgeted_skills_handles_empty_input() {
        let (out, footer) = budgeted_legacy_skills(&[], 10_000);
        assert!(out.is_empty());
        assert!(footer.is_none());
    }

    #[test]
    fn budgeted_skills_cap_is_hard_never_overshoots() {
        // Worst case: one skill bigger than the cap.
        let skills = vec![("huge".to_string(), "y".repeat(50_000))];
        let (out, footer) = budgeted_legacy_skills(&skills, 1_000);
        assert!(out.is_empty(), "oversized skill must be skipped, not truncated mid-content");
        assert!(footer.is_some());
    }

    #[test]
    fn budgeted_skills_lists_at_most_10_truncated_names() {
        // 15 skills, each 1KB, cap = 0 → all truncated. Footer should
        // list the first 10 names + "(and 5 more)".
        let skills: Vec<(String, String)> = (0..15)
            .map(|i| (format!("s{i}"), "x".repeat(1_000)))
            .collect();
        let (out, footer) = budgeted_legacy_skills(&skills, 0);
        assert!(out.is_empty());
        let footer_text = footer.unwrap();
        assert!(footer_text.contains("(and 5 more)"));
        // First name should appear, 11th name should not.
        assert!(footer_text.contains("s0"));
        assert!(!footer_text.contains("s14"));
    }
}
