//! Who may decide an approval card that belongs to a Personal employee
//! (native Linux isolation v1.81, task P1c). Protected mode only; see
//! `crate::personal_visibility` for the model.
//!
//! - **Dashboard** (`approvals.decide`, `mail.decide`):
//!   `handlers/personal_decide_gate.rs` runs the P1 gate with category
//!   [`DataCategory::Approval`](crate::personal_visibility::DataCategory):
//!   the owner decides; an Admin who is not the owner decides only with a
//!   fresh step-up and its `step_up_csrf`, after the audit row is written;
//!   everyone else is refused.
//! - **Channels** (button presses, chat text replies to a card, strict
//!   `確認 <id>` commands, the Telegram Mini App — every source that decides
//!   an `ApprovalBroker` card through `decision_notify::dispatch` or a bound
//!   request): no step-up exists there, so only the owner's own verified,
//!   active channel account decides ([`channel_card_refusal`],
//!   [`channel_agent_refusal`]). An Admin who is not the owner is refused
//!   and can decide from the dashboard with a step-up. P1d: the refused
//!   sender is told [`CHANNEL_CARD_NOT_FOUND`], the text of an unknown card.
//!
//! The card's employee is its `agent_id`; a card with a blank `agent_id`
//! falls back to the task it was built from (binding task, or the payload's
//! task reference), read from `tasks.db`. Ownership is the users.db mirror,
//! read fresh; any unreadable store refuses.
use std::path::Path;

use rusqlite::OptionalExtension;

use crate::approval::ApprovalRecord;
use crate::personal_visibility::{self as visibility, Unavailable};

/// The employee whose card `rec` is. `Ok(None)` = the card names no
/// employee and no task.
pub(crate) fn card_employee(home: &Path, rec: &ApprovalRecord) -> Result<Option<String>, Unavailable> {
    if !rec.agent_id.trim().is_empty() {
        return Ok(Some(rec.agent_id.clone()));
    }
    let Some(task) = rec
        .binding
        .as_ref()
        .and_then(|binding| binding.task_id.clone())
        .or_else(|| crate::handlers::task_privacy::approval_task_ref(&rec.payload))
        .filter(|task| !task.is_empty())
    else {
        return Ok(None);
    };
    let conn = crate::review_evidence::audience::readonly_db(&home.join("tasks.db")).map_err(|_| Unavailable)?;
    conn.query_row("SELECT assigned_to FROM tasks WHERE id=?1", [task.as_str()], |row| row.get::<_, String>(0))
        .optional()
        .map_err(|_| Unavailable)
}

/// Refusal for a decision on `employee`'s card by the person `decider`
/// resolves to (`None` = no verified person). Only the recorded owner
/// passes; the legacy `system` context never does. `decider` is called only
/// when the employee is a Personal employee. `None` = allowed (every
/// existing check still applies).
pub(crate) fn decider_refusal(
    home: &Path,
    protected: bool,
    employee: &str,
    decider: impl FnOnce() -> Option<String>,
) -> Option<&'static str> {
    if !protected || employee.is_empty() {
        return None;
    }
    match visibility::personal_owner(home, employee) {
        Ok(None) => None,
        Ok(Some(owner)) => {
            let decider = decider();
            let is_owner = decider
                .as_deref()
                .is_some_and(|user| !user.is_empty() && user != "system" && user == owner);
            (!is_owner).then_some(visibility::PERSONAL_DECISION_REFUSED)
        }
        Err(Unavailable) => Some(visibility::OWNERSHIP_UNAVAILABLE),
    }
}

/// P1d: what a channel sender who is not the owner sees — the text of an
/// unknown or expired card, so the reply never says whose card it is.
pub(crate) const CHANNEL_CARD_NOT_FOUND: &str = "找不到這筆核可（可能已過期並被清除）";

/// [`decider_refusal`] as a channel sender sees it.
fn channel_text(refusal: Option<&'static str>) -> Option<&'static str> {
    refusal.map(|message| if message == visibility::PERSONAL_DECISION_REFUSED { CHANNEL_CARD_NOT_FOUND } else { message })
}

/// The verified, active dashboard account behind a channel sender.
fn channel_decider(home: &Path, channel: &str, sender: &str) -> Option<String> {
    crate::trigger_route::access::verified_user_for_channel_sender(home, channel, sender)
}

/// [`channel_card_refusal`] with the protected-mode flag passed in (tests).
pub(crate) fn channel_card_refusal_with(
    home: &Path,
    protected: bool,
    rec: &ApprovalRecord,
    channel: &str,
    sender: &str,
) -> Option<&'static str> {
    if !protected {
        return None;
    }
    match card_employee(home, rec) {
        Ok(Some(employee)) => channel_text(decider_refusal(home, true, &employee, || channel_decider(home, channel, sender))),
        Ok(None) => None,
        Err(Unavailable) => Some(visibility::OWNERSHIP_UNAVAILABLE),
    }
}

/// A channel decision (press, text reply, Mini App, bound request) on
/// `rec`: refused unless the sender's verified account owns the card's
/// Personal employee. A no-op outside native Linux protected mode.
pub(crate) fn channel_card_refusal(
    home: &Path,
    rec: &ApprovalRecord,
    channel: &str,
    sender: &str,
) -> Option<&'static str> {
    channel_card_refusal_with(home, crate::runtime_readiness::isolation_active(), rec, channel, sender)
}

/// Like [`channel_card_refusal`] for decision cards keyed by their employee
/// (goal kickoff approvals and needs-human goal tasks).
pub(crate) fn channel_agent_refusal(home: &Path, agent: &str, channel: &str, sender: &str) -> Option<&'static str> {
    channel_text(decider_refusal(home, crate::runtime_readiness::isolation_active(), agent, || {
        channel_decider(home, channel, sender)
    }))
}
