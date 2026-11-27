//! Bound chat decisions retain the broker's immutable execution gates and
//! atomic receipt/ledger/resume write. Identity and receipt preflight are
//! already checked by the parent service under the shared decision lock.
use super::*;

pub(super) async fn decide(
    env: &DecisionEnv, rec: &ApprovalRecord, approve: bool,
    by: &Decider, actor: &DecisionActor,
) -> Result<Value, DecisionRefusal> {
    if crate::google_identity::member_card::is_member_card(rec) {
        return Err(DecisionRefusal::Forbidden("Only the requesting member may confirm".into()));
    }
    check_state(env, rec, false).await?;
    check_digest(rec)?;
    match (by, actor) {
        (Decider::ChatAdmin(_), DecisionActor::ChatAdmin(entry)) =>
            env.broker.decide_bound_chat_admin(&rec.id, entry, approve).await,
        (Decider::Dashboard(ctx), DecisionActor::Dashboard { .. })
            if rec.action_kind == crate::admin_ai::card::ACTION_KIND =>
            env.broker.decide_bound_dashboard(&rec.id, ctx, approve).await,
        _ => return Err(DecisionRefusal::NotSupportedInChat),
    }.map_err(DecisionRefusal::from_kind_error)?;
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
