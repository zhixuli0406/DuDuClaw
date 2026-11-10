//! Dashboard decisions on a Personal employee's approval card (native Linux
//! isolation v1.81, task P1c). Protected mode only; the model is in
//! `crate::personal_decision`.
//!
//! `approvals.decide` and `mail.decide` call
//! [`MethodHandler::personal_decide_gate`] once they have read the card and
//! before anything is decided: the card's employee
//! (`personal_decision::card_employee`) goes through the P1 gate with the
//! caller's live identity and category [`DataCategory::Approval`]. The owner
//! passes; an Admin who is not the owner passes only with a fresh step-up
//! and its `step_up_csrf` in the request, after the `auth_audit_log` row
//! (`isolation.personal_data_read`, `category = "approval"`, `method =
//! approvals.decide | mail.decide`) is written; without a step-up the Admin
//! is told one is needed ([`DECIDE_STEP_UP_REQUIRED`]). Everyone else is
//! refused ([`PERSONAL_DECISION_REFUSED`](visibility::PERSONAL_DECISION_REFUSED)).
use super::*;
use super::personal_data_gate::{NOT_ALLOWED, STEP_UP_REQUIRED, Target};
use crate::personal_visibility::{self as visibility, DataCategory};

/// Shown to an Admin who is not the owner and decided without a step-up.
pub(super) const DECIDE_STEP_UP_REQUIRED: &str = "這張審核卡屬於個人專屬員工。管理員需先以密碼與雙因素驗證確認身分（有效 5 分鐘），\
    並在決定請求中帶入驗證後的 step_up_csrf；每次決定都會寫入稽核紀錄。 / This approval belongs to a Personal employee. \
    An Admin who is not its owner needs a fresh password + TOTP step-up and its step_up_csrf token to decide it; every \
    decision is audited.";

/// The text a decider sees for a refusal of the shared P1 gate.
pub(super) fn decide_refusal_text(message: &'static str) -> &'static str {
    if message == STEP_UP_REQUIRED {
        DECIDE_STEP_UP_REQUIRED
    } else if message == NOT_ALLOWED {
        visibility::PERSONAL_DECISION_REFUSED
    } else {
        message
    }
}

impl MethodHandler {
    /// Owner / audited Admin step-up / refused, for one card. `method` names
    /// the RPC in the audit row. A no-op outside protected mode and for
    /// cards of Shared employees.
    pub(super) async fn personal_decide_gate(
        &self,
        ctx: &UserContext,
        record: &crate::approval::ApprovalRecord,
        csrf: Option<[u8; 32]>,
        method: &'static str,
    ) -> Result<(), WsFrame> {
        if !crate::runtime_readiness::isolation_active() {
            return Ok(());
        }
        let refuse = |message: &str| WsFrame::error_response("", message);
        let (home, rec, session) = (self.home_dir.clone(), record.clone(), ctx.clone());
        let (employee, live) = tokio::task::spawn_blocking(move || {
            let employee = crate::personal_decision::card_employee(&home, &rec);
            let live = super::task_privacy::live_reader_context(&home, &session);
            (employee, live)
        })
        .await
        .map_err(|_| refuse(visibility::OWNERSHIP_UNAVAILABLE))?;
        let Some(agent) = employee.map_err(|_| refuse(visibility::OWNERSHIP_UNAVAILABLE))? else {
            return Ok(());
        };
        // Role, status and the person behind the session come from users.db
        // now, not from the handshake.
        let live = live.map_err(|()| refuse(super::task_privacy::PERMISSION_DENIED))?;
        let target = Target { method, category: DataCategory::Approval, agent: &agent };
        self.personal_data_gate(&live, target, csrf)
            .await
            .map_err(|message| refuse(decide_refusal_text(message)))
    }
}

#[cfg(all(test, unix))]
#[path = "personal_p1c_tests.rs"]
mod tests;
