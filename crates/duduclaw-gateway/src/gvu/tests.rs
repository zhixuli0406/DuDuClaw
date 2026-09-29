//! Unit tests for the GVU evolution loop.
//!
//! S11 (2026-09-29): every module that exercised the legacy SOUL.md
//! rewrite cycle (proposal / verifier chain / version store observation
//! windows / updater / generator / soul patches / consolidate) went with
//! the code it tested. What remains is the machinery AEE still uses.

#[cfg(test)]
mod text_gradient_tests {
    use crate::gvu::text_gradient::TextGradient;

    #[test]
    fn blocking_gradient_format() {
        let g = TextGradient::blocking("L1", "SOUL.md", "contains secret", "remove the secret");
        let section = g.to_prompt_section();
        assert!(section.contains("[BLOCKING]"));
        assert!(section.contains("L1"));
        assert!(section.contains("contains secret"));
        assert!(section.contains("remove the secret"));
    }

    #[test]
    fn advisory_gradient_format() {
        let g = TextGradient::advisory(
            "L2",
            "direction",
            "oscillation detected",
            "pick one direction",
        );
        let section = g.to_prompt_section();
        assert!(section.contains("[ADVISORY]"));
    }
}

#[cfg(test)]
mod experiment_log_tests {
    use crate::gvu::version_store::*;
    use std::time::Duration;

    #[test]
    fn record_and_query_experiment() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let entry = ExperimentLogEntry::new(
            "agent1",
            3,
            5,
            Duration::from_secs(120),
            "applied",
            "Approved at generation 3",
        );
        vs.record_experiment(&entry);

        let logs = vs.get_experiments("agent1", 10);
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].agent_id, "agent1");
        assert_eq!(logs[0].generations_used, 3);
        assert_eq!(logs[0].generations_budget, 5);
        assert_eq!(logs[0].outcome, "applied");
        assert!((logs[0].duration_secs - 120.0).abs() < 0.1);
    }

    #[test]
    fn experiment_summary_counts() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        // Record various outcomes
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1",
            2,
            3,
            Duration::from_secs(60),
            "applied",
            "ok",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1",
            3,
            3,
            Duration::from_secs(90),
            "abandoned",
            "failed",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1",
            3,
            3,
            Duration::from_secs(80),
            "deferred",
            "retry later",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1",
            1,
            3,
            Duration::from_secs(30),
            "timed_out",
            "timeout",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1",
            0,
            3,
            Duration::from_secs(1),
            "skipped",
            "observation active",
        ));

        let summary = vs.get_experiment_summary("agent1");
        assert_eq!(summary.total_experiments, 5);
        assert_eq!(summary.applied_count, 1);
        assert_eq!(summary.abandoned_count, 1);
        assert_eq!(summary.deferred_count, 1);
        assert_eq!(summary.timed_out_count, 1);
        assert_eq!(summary.skipped_count, 1);
        // success_rate = 1 applied / 4 actionable (5 total - 1 skipped)
        assert!((summary.success_rate - 0.25).abs() < 0.01);
    }

    #[test]
    fn experiment_summary_empty_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let summary = vs.get_experiment_summary("nonexistent");
        assert_eq!(summary.total_experiments, 0);
        assert_eq!(summary.success_rate, 0.0);
    }

    #[test]
    fn experiments_newest_first() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        let e1 =
            ExperimentLogEntry::new("agent1", 1, 3, Duration::from_secs(10), "applied", "first");
        // Small delay to ensure different timestamps
        std::thread::sleep(Duration::from_millis(10));
        let e2 = ExperimentLogEntry::new(
            "agent1",
            2,
            3,
            Duration::from_secs(20),
            "abandoned",
            "second",
        );

        vs.record_experiment(&e1);
        vs.record_experiment(&e2);

        let logs = vs.get_experiments("agent1", 10);
        assert_eq!(logs.len(), 2);
        assert_eq!(logs[0].description, "second"); // newest first
        assert_eq!(logs[1].description, "first");
    }

    #[test]
    fn experiments_respects_limit() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        for i in 0..10 {
            vs.record_experiment(&ExperimentLogEntry::new(
                "agent1",
                i,
                3,
                Duration::from_secs(10),
                "applied",
                &format!("exp-{i}"),
            ));
        }

        let logs = vs.get_experiments("agent1", 3);
        assert_eq!(logs.len(), 3);
    }

    #[test]
    fn experiments_isolated_per_agent() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let vs = VersionStore::new(tmp.path());

        vs.record_experiment(&ExperimentLogEntry::new(
            "agent1",
            1,
            3,
            Duration::from_secs(10),
            "applied",
            "a1",
        ));
        vs.record_experiment(&ExperimentLogEntry::new(
            "agent2",
            2,
            3,
            Duration::from_secs(20),
            "abandoned",
            "a2",
        ));

        assert_eq!(vs.get_experiments("agent1", 10).len(), 1);
        assert_eq!(vs.get_experiments("agent2", 10).len(), 1);
        assert_eq!(vs.get_experiment_summary("agent1").applied_count, 1);
        assert_eq!(vs.get_experiment_summary("agent2").abandoned_count, 1);
    }
}
