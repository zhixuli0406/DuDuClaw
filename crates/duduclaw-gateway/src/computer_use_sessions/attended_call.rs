//! Per-runtime attendance, independent of principal propagation. A live turn
//! identifier alone is not evidence about the runtime making this request.
use super::attended::AttendedInputProof;
use std::{
    future::Future,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub(crate) struct AttendedCall {
    agent: String,
    turn: String,
    proof: AttendedInputProof,
    runtime_live: Arc<AtomicBool>,
}
tokio::task_local! {
    static MAY_MINT: bool;
    static CURRENT_CALL: Option<AttendedCall>;
}

pub(crate) fn runtime_scope<F: Future>(
    allowed: bool,
    future: F,
) -> impl Future<Output = F::Output> {
    let future = Box::pin(future);
    async move {
        let allowed = allowed && MAY_MINT.try_with(|outer| *outer).unwrap_or(true);
        MAY_MINT.scope(allowed, future).await
    }
}
pub(crate) fn request_scope<F: Future>(
    call: Option<AttendedCall>,
    future: F,
) -> impl Future<Output = F::Output> {
    CURRENT_CALL.scope(call, Box::pin(future))
}
pub(crate) fn capture(agent: &str) -> Option<AttendedCall> {
    if !MAY_MINT.try_with(|allowed| *allowed).unwrap_or(true)
        || crate::memory_provenance::DISPATCH_RUN
            .try_with(|run| run.is_some())
            .unwrap_or(false)
        || crate::runtime::round_task_env().is_some()
        || crate::explore_lane::in_explore()
        || crate::claude_runner::REPLY_SENDER
            .try_with(Clone::clone)
            .ok()
            .is_some_and(|sender| {
                duduclaw_core::delegation_policy::is_system_sender(&sender)
                    || duduclaw_core::delegation_policy::is_reserved_agent_id(&sender)
            })
    {
        return None;
    }
    let turn = duduclaw_memory::feedback::CURRENT_TURN_ID
        .try_with(Clone::clone)
        .ok()
        .flatten()?;
    let proof = super::attended::capture(agent, &turn)?;
    Some(AttendedCall {
        agent: agent.into(),
        turn,
        proof,
        runtime_live: Arc::new(AtomicBool::new(true)),
    })
}
impl AttendedCall {
    /// Read-only comparison of existing host-issued runtime leases. A new
    /// transport context never inherits consent merely by naming the same turn.
    pub(crate) fn same_live_call(&self, other: &Self) -> bool {
        self.agent == other.agent
            && self.turn == other.turn
            && Arc::ptr_eq(&self.runtime_live, &other.runtime_live)
            && self.valid_for(&self.agent)
            && other.valid_for(&other.agent)
    }
    pub(crate) fn valid_for(&self, agent: &str) -> bool {
        self.runtime_live.load(Ordering::Acquire)
            && self.agent == agent
            && self.proof.valid_for(agent, &self.turn)
    }
    pub(crate) fn invalidate(&self) {
        self.runtime_live.store(false, Ordering::Release);
    }
    pub(crate) fn matches_turn(&self, agent: &str, turn: &str) -> bool {
        self.valid_for(agent) && self.turn == turn
    }
}
pub(crate) fn current(agent: &str) -> bool {
    CURRENT_CALL
        .try_with(|call| call.as_ref().is_some_and(|call| call.valid_for(agent)))
        .unwrap_or(false)
}
pub(crate) fn current_call(agent: &str) -> Option<AttendedCall> {
    CURRENT_CALL
        .try_with(|call| call.as_ref().filter(|call| call.valid_for(agent)).cloned())
        .ok()
        .flatten()
}
pub(crate) fn current_turn(agent: &str) -> Option<String> {
    CURRENT_CALL
        .try_with(|call| {
            call.as_ref()
                .filter(|call| call.valid_for(agent))
                .map(|call| call.turn.clone())
        })
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn background_scope_cannot_mint_from_inherited_live_human_turn_and_principal() {
        let agent = "local-source-agent";
        let session = "line:local-source";
        let call = crate::computer_use_sessions::attended::verified_channel_message(
            session,
            "human",
            false,
            async {
                assert!(crate::computer_use_sessions::attended::bind_final_route(
                    session,
                    session,
                    agent,
                    "live-turn"
                ));
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(
                        Some(session.into()),
                        duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                            Some("live-turn".into()),
                            async {
                                let call = capture(agent).unwrap();
                                assert!(call.valid_for(agent));
                                assert!(!call.valid_for("different-agent"));
                                for source in ["dispatch", "goal", "heartbeat", "workflow"] {
                                    runtime_scope(
                                        false,
                                        crate::on_behalf_of::ON_BEHALF_OF.scope(
                                            Some("person:owner".into()),
                                            async {
                                                assert!(capture(agent).is_none(), "{source}");
                                                runtime_scope(true, async {
                                                    assert!(
                                                        capture(agent).is_none(),
                                                        "nested chat cannot widen {source}"
                                                    );
                                                })
                                                .await;
                                            },
                                        ),
                                    )
                                    .await;
                                }
                                crate::memory_provenance::DISPATCH_RUN
                                    .scope(Some(("dispatch-session".into(), "run".into())), async {
                                        assert!(capture(agent).is_none());
                                    })
                                    .await;
                                request_scope(Some(call.clone()), async {
                                    assert!(current(agent));
                                })
                                .await;
                                request_scope(None, async {
                                    assert!(!current(agent));
                                })
                                .await;
                                call
                            },
                        ),
                    )
                    .await
            },
        )
        .await;
        assert!(!call.valid_for(agent), "proof outlived ingress");
    }
}
