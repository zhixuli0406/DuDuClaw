//! Regression guard for the runner's async state-machine size (F3 item 14).
//!
//! The runner once overflowed tokio's 2 MiB worker stack: the nested step
//! handlers were inlined into one state machine. F1a boxed them
//! (`Box::pin` in `runner.rs`). These tests pin the size of the top-level
//! futures so that removing the boxing fails here, not as a stack overflow
//! at run time. They only build the futures; nothing is polled.
use super::runner_tests::process_fixture;

/// Limits per future, measured 2026-10-06 (debug build): boxed / with the
/// `Box::pin` calls in `runner.rs` removed. `runner.execute` 1,688 / 8,072
/// bytes; `service.run_fixture` 6,600 / 12,560; `service.sweep` 5,752 (does
/// not go through the step handlers). Each limit sits between the two.
const LIMITS: &[(&str, usize)] = &[
    ("runner.execute", 4 * 1024),
    ("service.run_fixture", 9 * 1024),
    ("service.sweep", 8 * 1024),
];

#[tokio::test]
async fn runner_top_level_futures_stay_small() {
    let (_home, service, request, _context) = process_fixture().await;
    let sizes = [
        (
            "runner.execute",
            std::mem::size_of_val(&service.runner.execute("run")),
        ),
        (
            "service.run_fixture",
            std::mem::size_of_val(&service.run_fixture(request)),
        ),
        ("service.sweep", std::mem::size_of_val(&service.sweep())),
    ];
    for (name, size) in sizes {
        eprintln!("{name}: {size} bytes");
        let limit = LIMITS.iter().find(|(n, _)| *n == name).unwrap().1;
        assert!(
            size <= limit,
            "{name} future is {size} bytes; keep the step handlers boxed (Box::pin)"
        );
    }
}
