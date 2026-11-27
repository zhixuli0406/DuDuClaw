//! Bound chat decisions retain the broker's immutable execution gates and
//! atomic receipt/ledger/resume write. Identity and receipt preflight are
//! already checked by the parent service under the shared decision lock.
use super::*;

pub(super) async fn decide(
    env: &DecisionEnv, rec: &ApprovalRecord, approve: bool,
    by: &Decider, actor: &DecisionActor,
) -> Result<Value, DecisionRefusal> {
    let DecisionActor::ChatAdmin(entry) = actor else {
        return Err(DecisionRefusal::NotSupportedInChat);
    };
    check_state(env, rec, false).await?;
    check_digest(rec)?;
    env.broker.decide_bound_chat_admin(&rec.id, entry, approve).await
        .map_err(DecisionRefusal::from_kind_error)?;
    crate::handlers::approval_decider::audit_activation_decision(
        &env.home, rec, by.user_id(), approve,
    );
    let name = identity::display_name(env, by.user_id()).await;
    crate::approval_notify::spawn_dashboard_collapse(env.home.clone(), rec.clone(), approve, name);
    Ok(json!({
        "id": rec.id.as_str(),
        "decided": if approve { "approved" } else { "denied" },
        "execution_status": "awaiting_execution",
        "submitter_is_decider": crate::handlers::approval_decider::submitter_is_decider_id(rec, by.user_id()),
    }))
}
