//! H13b (D-H3, D8, owner decision A 2026-11-09): the background run of a
//! protected `spawn_ephemeral` child.
//!
//! The child is ordinary delegated background work of the PARENT employee
//! itself (same employee id, incarnation, binding, uid and storage):
//!
//! 1. While the parent run is live, `isolation_ephemeral::queue_for_parent`
//!    captures a `delegation` receipt for it through the existing
//!    `record_delegation_inner` path (live run association, fresh lifecycle
//!    and users.db reads; target = source). Its id is [`message_id`] of the
//!    job, its material is [`child_material`] — both derived only from the
//!    stored job, so the start can rebuild them exactly.
//! 2. After the parent run exits, `isolation_ephemeral_start` releases the job
//!    (`take_startable`, one-shot) and enqueues [`child_message`] on the
//!    SQLite message queue: sender = target = the parent's label, on behalf of
//!    the parent's verified requester.
//! 3. The dispatcher gate admits that self-delegation only for a released job
//!    whose employee id and incarnation equal the live ones
//!    (`isolation_ephemeral_start::self_delegation_allowed`); `scope_record` /
//!    `recheck` then authorize it like any delegation (B1 admits a Personal
//!    parent only through `same_owner`), and `authorize_current` narrows the
//!    child's tool snapshot to the job's tools ([`child_tools`]).
//!
//! The child itself never files another child (depth 1, [`is_ephemeral_child`]).
//! Its `tier` is recorded on the job only: the child uses the parent's model
//! routing (documented H13 behaviour).
use std::path::Path;
use serde_json::Value;
use duduclaw_isolation::{bounded::{List, Text}, capability::Lane};
use duduclaw_mcp_boundary::{RequestContext, VerificationError};
use crate::isolation_ephemeral::EphemeralJob;
use super::RecordAuthority;

/// Queue message id (= receipt id) of a job: this prefix + the job's uuid.
pub const MESSAGE_PREFIX: &str = "ephemeral-";
/// Delegation depth of every child (the parent run is depth 0 for its own
/// spawn; a child cannot spawn another child).
pub const CHILD_DEPTH: u8 = 1;

pub fn message_id(job_id: &str) -> String { format!("{MESSAGE_PREFIX}{job_id}") }

/// The job id named by a child message id: exactly [`MESSAGE_PREFIX`] + a
/// canonical lowercase hyphenated uuid (what `EphemeralJob::new` mints).
pub fn job_id_of(message_id: &str) -> Option<&str> {
    let id = message_id.strip_prefix(MESSAGE_PREFIX)?;
    let parsed = uuid::Uuid::parse_str(id).ok()?;
    (parsed.hyphenated().to_string() == id).then_some(id)
}

/// The wire principal the child works for: the parent run's verified human.
pub fn owner_principal(job: &EphemeralJob) -> Result<String, VerificationError> {
    duduclaw_core::HumanPrincipal::user(&job.parent.requester).map(|principal| principal.to_string())
        .ok_or(VerificationError::Invalid)
}

fn lane(job: &EphemeralJob) -> Option<&'static str> { (job.parent.lane == Lane::Explore).then_some("explore") }

/// The child's prompt: the instruction, then the context. Bounded like any
/// bus payload.
pub fn child_prompt(job: &EphemeralJob) -> Result<String, VerificationError> {
    let prompt = format!("{}\n\n{}", job.instruction.trim(), job.context.trim());
    if prompt.len() > duduclaw_mcp_boundary::tool_policy::spawn::SPAWN_TEXT_MAX_BYTES {
        return Err(VerificationError::Invalid);
    }
    Ok(prompt)
}

/// The delegation material the receipt binds, equal to what the dispatcher
/// (`isolated_queue_scope`) and `material::read_current` derive from the
/// stored queue row: the queue masks credentials on insert, so the bound
/// payload is the masked prompt.
pub fn child_material(job: &EphemeralJob) -> Result<Value, VerificationError> {
    let agent = job.parent.agent.as_str();
    let payload = duduclaw_security::credential_detect::mask(&child_prompt(job)?);
    let owner = owner_principal(job)?;
    Ok(super::material::delegation(agent, agent, &payload, None, None, lane(job), CHILD_DEPTH, Some(agent),
        None, Some(agent), false, 0, false, Some(&owner)))
}

/// The SQLite queue row that starts the child (fields mirror [`child_material`]).
pub fn child_message(job: &EphemeralJob) -> Result<crate::message_queue::QueueMessage, VerificationError> {
    let agent = job.parent.agent.clone();
    Ok(crate::message_queue::QueueMessage {
        id: message_id(&job.id), sender: agent.clone(), target: agent.clone(), payload: child_prompt(job)?,
        status: crate::message_queue::MessageStatus::Pending, retry_count: 0, delegation_depth: i32::from(CHILD_DEPTH),
        origin_agent: Some(agent.clone()), sender_agent: Some(agent), error: None, response: None,
        created_at: chrono::Utc::now().to_rfc3339(), acked_at: None, completed_at: None, reply_channel: None,
        turn_id: None, session_id: None, upstream_unknown: false, lane: lane(job).map(str::to_owned),
        on_behalf_of: Some(owner_principal(job)?),
    })
}

/// The receipt of an ephemeral child: a delegation from the employee to
/// itself (same id and incarnation) under a child message id. Returns the
/// job id.
pub fn ephemeral_job_of(receipt: &RecordAuthority) -> Option<&str> {
    (receipt.kind == "delegation" && receipt.parent_agent.as_deref() == Some(receipt.agent.as_str())
        && receipt.caller == Some(receipt.employee) && receipt.caller_incarnation == receipt.incarnation)
        .then(|| job_id_of(&receipt.id)).flatten()
}

/// The run of `receipt` is itself an ephemeral child (it may not file one).
pub fn is_ephemeral_child(receipt: &RecordAuthority) -> bool { ephemeral_job_of(receipt).is_some() }

/// The job equals the receipt's identity exactly: label, employee id,
/// incarnation, binding, parent run and requester.
pub(crate) fn job_matches_receipt(job: &EphemeralJob, receipt: &RecordAuthority) -> Result<(), VerificationError> {
    if job.parent.agent != receipt.agent || job.parent.employee != receipt.employee
        || job.parent.incarnation != receipt.incarnation || receipt.binding.as_ref() != Some(&job.parent.binding)
        || receipt.parent_run != Some(job.parent.run) || owner_principal(job)? != receipt.owner {
        return Err(VerificationError::BindingMismatch);
    }
    Ok(())
}

/// The granted tools that the job also names, in grant order: never more
/// than the job (requested ∩ parent snapshot) nor than the child's own live
/// tool snapshot. Empty refuses.
pub fn narrowed_tools(granted: &[Text<128>], job_tools: &[String])
    -> Result<List<Text<128>, 256>, VerificationError> {
    let tools: Vec<Text<128>> = granted.iter()
        .filter(|tool| job_tools.iter().any(|name| name == tool.as_str())).cloned().collect();
    if tools.is_empty() { return Err(VerificationError::Invalid); }
    List::new(tools).map_err(|_| VerificationError::Invalid)
}

/// `authorize_current`'s narrowing. `None`: not a self-delegation, nothing to
/// narrow. A self-delegation must be the receipt of a released job that
/// matches it exactly (`released` reads `isolation-ephemeral-jobs.db`);
/// anything else refuses the run.
pub fn child_tools(receipt: &RecordAuthority, granted: &[Text<128>],
    released: impl FnOnce(&str) -> Result<Option<EphemeralJob>, VerificationError>)
    -> Result<Option<List<Text<128>, 256>>, VerificationError> {
    if receipt.kind != "delegation" || receipt.parent_agent.as_deref() != Some(receipt.agent.as_str()) {
        return Ok(None);
    }
    let id = ephemeral_job_of(receipt).ok_or(VerificationError::BindingMismatch)?;
    let job = released(id)?.ok_or(VerificationError::Revoked)?;
    job_matches_receipt(&job, receipt)?;
    narrowed_tools(granted, &job.tools).map(Some)
}

/// Capture the child's delegation receipt from the LIVE parent run (step 1).
/// Personal parent: `same_owner` requires the requester to be the lifecycle
/// owner (fresh lifecycle + users.db read) and no owner card is filed.
pub async fn record_child(home: &Path, sender: &str, context: &RequestContext, job: &EphemeralJob)
    -> Result<(), VerificationError> {
    if job.parent.agent != sender { return Err(VerificationError::BindingMismatch); }
    let material = child_material(job)?;
    super::record_delegation_inner(home, sender, context, &job.parent.agent, &message_id(&job.id), &material, false)
        .await.map_err(|error| match error {
            super::DelegationError::Failed(error) => error,
            // Never an owner card for a self-delegation.
            _ => VerificationError::BindingMismatch,
        })
}

#[cfg(test)]
#[path = "ephemeral_child_tests.rs"]
mod tests;
