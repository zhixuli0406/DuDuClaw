//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::goal_task_settle_verb;
use crate::decision_card::DecisionVerb;

#[test]
fn maps_the_three_legal_goal_loop_outcomes() {
    assert_eq!(
        goal_task_settle_verb("pending"),
        Some(DecisionVerb::Retried)
    );
    assert_eq!(
        goal_task_settle_verb("done"),
        Some(DecisionVerb::MarkedDone)
    );
    assert_eq!(
        goal_task_settle_verb("cancelled"),
        Some(DecisionVerb::Abandoned)
    );
}

#[test]
fn refuses_to_guess_at_anything_else() {
    // in_progress/blocked/needs_human/unknown-status: not a legal
    // needs_human resolution — must not collapse the card with a
    // fabricated verb.
    for s in [
        "in_progress",
        "blocked",
        "needs_human",
        "review",
        "",
        "garbage",
    ] {
        assert_eq!(
            goal_task_settle_verb(s),
            None,
            "status {s:?} must not map to a verb"
        );
    }
}
