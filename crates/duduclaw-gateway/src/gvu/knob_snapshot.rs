//! WP-6A / A2 — harness-knob telemetry snapshot (read-only, zero behavior
//! change).
//!
//! `commercial/docs/DESIGN-evolution-harness-knobs-2026-08.md` §0 concludes
//! that letting the evolution engine *tune* these knobs is SHELVED — see the
//! design doc's §6.8 for why an automatic loop is never the right tool for
//! any of them. What the document DOES recommend as a cheap, zero-risk
//! precondition (§7.2-A2) is recording the *current* value of every knob
//! named in its §1.2 table alongside telemetry DuDuClaw already emits, so a
//! future retrospective analysis (or an eventual operator-run
//! `duduclaw knobs sweep`, §7.2-A1, not yet built) has historical data to
//! compare against.
//!
//! This module is exactly that and nothing more: a **read-only,
//! side-effect-free** snapshot. It never writes to any config file, never
//! gates any decision, and is not consulted by any live code path — only
//! attached to outgoing telemetry records. Every read degrades to that
//! knob's own hard-coded default on a missing/malformed config file or
//! section, matching the fail-open posture the knobs' own
//! `from_home`/`from_agent_dir` readers already use.
//!
//! `two_stage_judge` is read directly against `config.toml [dispatch]`
//! rather than reusing `dispatch_engine::TwoStageJudgeConfig::from_home`
//! (private to that module, and this module must never gain any call-in
//! surface that could influence a dispatch decision — only observe the same
//! value that path already reads).

use std::path::Path;

use serde::Serialize;

use super::verifier_measure::NoiseBand;

/// `[dispatch] two_stage_judge` default — mirrors
/// `dispatch_engine::TwoStageJudgeConfig::default()`. Duplicated here (that
/// struct is private to `dispatch_engine`) rather than made `pub`, so this
/// telemetry-only module can never become a second source of truth the
/// dispatch path might accidentally read from.
const TWO_STAGE_JUDGE_DEFAULT: bool = true;

/// One point-in-time reading of every harness knob named in the design
/// doc's §1.2 table. Attached to outgoing telemetry only — see module docs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct KnobSnapshot {
    // ── `config.toml [goal_loop]` — global; `GoalLoopConfig` has no
    // per-agent override, see `goal_loop.rs::GoalLoopConfig::from_home`. ──
    pub iteration_cap: u32,
    pub iteration_cap_simple: u32,
    pub soft_cap: i64,
    pub wall_clock_hours: i64,
    pub progress_report_minutes: i64,
    /// Raw string, not the parsed `ResumeOnRestart` enum — an unrecognised
    /// value is itself useful telemetry (shows what the operator actually
    /// typed, even though the loop treats it as the safe default).
    pub resume_on_restart: String,
    pub tool_streak_advisory: bool,

    // ── `agent.toml [evolution] noise_band` — per agent. ───────────────────
    pub noise_band_cases: f64,
    pub noise_band_holdout: f64,
    pub noise_band_judge: f64,
    pub noise_band_novelty: f64,
    pub noise_band_relevance: f64,

    // ── `config.toml [dispatch]` — global. ─────────────────────────────────
    pub two_stage_judge: bool,
    pub judge_mode: &'static str,
    pub admission_mode: &'static str,
}

/// Capture the current value of every tracked knob for `agent_dir`
/// (per-agent knobs) against `home_dir` (global `config.toml` knobs).
///
/// Every sub-read already degrades to its own safe default on a missing or
/// malformed file — this function adds no new failure mode on top of them.
pub fn capture(home_dir: &Path, agent_dir: &Path) -> KnobSnapshot {
    let goal_loop = crate::goal_loop::GoalLoopConfig::from_home(home_dir);
    let band = NoiseBand::from_agent_dir(agent_dir);
    let judge_mode = crate::judge_mode::JudgeMode::from_home(Some(home_dir));
    let admission = duduclaw_core::spawn_admission::AdmissionConfig::from_home(home_dir);

    KnobSnapshot {
        iteration_cap: goal_loop.iteration_cap,
        iteration_cap_simple: goal_loop.iteration_cap_simple,
        soft_cap: goal_loop.soft_cap,
        wall_clock_hours: goal_loop.wall_clock_hours,
        progress_report_minutes: goal_loop.progress_report_minutes,
        resume_on_restart: goal_loop.resume_on_restart,
        tool_streak_advisory: goal_loop.tool_streak_advisory,

        noise_band_cases: band.cases,
        noise_band_holdout: band.holdout,
        noise_band_judge: band.judge,
        noise_band_novelty: band.novelty,
        noise_band_relevance: band.relevance,

        two_stage_judge: two_stage_judge_enabled(home_dir),
        judge_mode: judge_mode.as_str(),
        admission_mode: match admission.admission {
            duduclaw_core::spawn_admission::AdmissionMode::Queue => "queue",
            duduclaw_core::spawn_admission::AdmissionMode::Fail => "fail",
        },
    }
}

/// Read `[dispatch] two_stage_judge` in isolation. Mirrors (but does not
/// call — see module docs) `dispatch_engine::TwoStageJudgeConfig::from_home`.
fn two_stage_judge_enabled(home_dir: &Path) -> bool {
    let Ok(content) = std::fs::read_to_string(home_dir.join("config.toml")) else {
        return TWO_STAGE_JUDGE_DEFAULT;
    };
    let Ok(table) = content.parse::<toml::Table>() else {
        return TWO_STAGE_JUDGE_DEFAULT;
    };
    table
        .get("dispatch")
        .and_then(|v| v.as_table())
        .and_then(|t| t.get("two_stage_judge"))
        .and_then(|v| v.as_bool())
        .unwrap_or(TWO_STAGE_JUDGE_DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn absent_config_and_agent_dir_yields_all_defaults() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = tempfile::tempdir().unwrap();
        let snap = capture(home.path(), agent_dir.path());

        assert_eq!(snap.iteration_cap, 5);
        assert_eq!(snap.iteration_cap_simple, 3);
        assert_eq!(snap.soft_cap, 3);
        assert_eq!(snap.wall_clock_hours, 24);
        assert_eq!(snap.progress_report_minutes, 10);
        assert_eq!(snap.resume_on_restart, "pause");
        assert!(snap.tool_streak_advisory);

        assert_eq!(snap.noise_band_cases, 0.05);
        assert_eq!(snap.noise_band_judge, 0.15);

        assert!(snap.two_stage_judge);
        assert_eq!(snap.judge_mode, "mav");
        assert_eq!(snap.admission_mode, "queue");
    }

    #[test]
    fn reads_overrides_from_both_files() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = tempfile::tempdir().unwrap();

        fs::write(
            home.path().join("config.toml"),
            "[goal_loop]\niteration_cap = 8\nresume_on_restart = \"auto\"\n\n\
             [dispatch]\ntwo_stage_judge = false\njudge = \"human_only\"\nadmission = \"fail\"\n",
        )
        .unwrap();
        fs::write(
            agent_dir.path().join("agent.toml"),
            "[evolution.noise_band]\ncases = 0.08\n",
        )
        .unwrap();

        let snap = capture(home.path(), agent_dir.path());
        assert_eq!(snap.iteration_cap, 8);
        assert_eq!(snap.resume_on_restart, "auto");
        assert!(!snap.two_stage_judge);
        assert_eq!(snap.judge_mode, "human_only");
        assert_eq!(snap.admission_mode, "fail");
        assert_eq!(snap.noise_band_cases, 0.08);
        // holdout re-derives from the overridden cases band (half of it).
        assert_eq!(snap.noise_band_holdout, 0.04);
    }

    #[test]
    fn malformed_files_degrade_to_defaults_not_a_panic() {
        let home = tempfile::tempdir().unwrap();
        let agent_dir = tempfile::tempdir().unwrap();
        fs::write(home.path().join("config.toml"), "not valid toml {{{").unwrap();
        fs::write(agent_dir.path().join("agent.toml"), "also not valid [[[").unwrap();

        let snap = capture(home.path(), agent_dir.path());
        assert_eq!(snap.iteration_cap, 5);
        assert!(snap.two_stage_judge);
        assert_eq!(snap.judge_mode, "mav");
    }
}
