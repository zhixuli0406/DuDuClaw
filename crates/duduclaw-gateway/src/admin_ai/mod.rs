//! Administrator settings access through a live, attended avatar turn.
pub mod route;
mod decision;
pub(crate) use decision::{may_decide, may_decide_chat, decided_by_admin, receipt_matches, REFUSAL};
pub(crate) mod proposal;
pub(crate) mod card;
pub(crate) mod execute;
