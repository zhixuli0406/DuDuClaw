//! Proof that an actual verified inbound human message is still being handled.
//! Only native/channel adapters enter this scope. It is not deserializable and
//! cannot be reconstructed from a principal, session name or tool argument.
use std::{
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone)]
pub(crate) struct AttendedInputProof {
    session: String,
    live: Arc<AtomicBool>,
    bound: Arc<Mutex<Option<(String, String)>>>,
    final_route: Arc<Mutex<Option<FinalRoute>>>,
}

#[derive(Clone, PartialEq, Eq)]
struct FinalRoute {
    session: String,
    agent: String,
    turn: String,
}
impl AttendedInputProof {
    pub(crate) fn valid_for(&self, agent: &str, turn: &str) -> bool {
        self.live.load(Ordering::Acquire)
            && self
                .bound
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .is_some_and(|(a, t)| a == agent && t == turn)
            && self
                .final_route
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .is_some_and(|route| route.agent == agent && route.turn == turn)
    }
}
tokio::task_local! { static VERIFIED_INPUT: Option<AttendedInputProof>; }
struct InputLifetime(Arc<AtomicBool>);
impl Drop for InputLifetime {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

fn real_sender(sender: &str) -> bool {
    !sender.is_empty()
        && !sender.chars().any(char::is_control)
        && !duduclaw_core::delegation_policy::is_system_sender(sender)
        && !duduclaw_core::delegation_policy::is_reserved_agent_id(sender)
        && sender != "system"
}

/// Call exclusively after the adapter has authenticated an inbound MESSAGE
/// and classified its sender. Reply continuations, approvals and scheduled
/// jobs must not call this, even when they retain a TrustedReplyTarget.
pub(crate) fn verified_channel_message<F: Future>(
    session: &str,
    sender: &str,
    bot: bool,
    future: F,
) -> impl Future<Output = F::Output> {
    let recognized = crate::channel_reply::is_messaging_session(session)
        && !crate::odoo_livechat::is_public_visitor_session(session)
        && !session.starts_with("widget:")
        && real_sender(sender)
        && !bot;
    scope(recognized.then(|| session.to_owned()), future)
}

/// `principal` must be the authenticated socket principal, before the turn
/// builder (public widget visitors have no such proof).
pub(crate) fn verified_webchat_message<F: Future>(
    session: &str,
    principal: Option<&str>,
    future: F,
) -> impl Future<Output = F::Output> {
    let recognized = session.starts_with("webchat:")
        && principal.is_some_and(|p| real_sender(p) && !p.starts_with("widget-visitor:"));
    scope(recognized.then(|| session.to_owned()), future)
}

fn scope<F: Future>(session: Option<String>, future: F) -> impl Future<Output = F::Output> {
    // Box before constructing another task-local wrapper, preserving the
    // gateway's debug-build future/stack-size contract.
    let future = Box::pin(future);
    async move {
        let proof = session.map(|session| AttendedInputProof {
            session,
            live: Arc::new(AtomicBool::new(true)),
            bound: Arc::new(Mutex::new(None)),
            final_route: Arc::new(Mutex::new(None)),
        });
        let _lifetime = proof.as_ref().map(|p| InputLifetime(p.live.clone()));
        VERIFIED_INPUT.scope(proof, future).await
    }
}

/// Called by the reply pipeline ONLY after its trusted routing decision.
/// The authenticated conversation is an exact anchor, not a prefix rule.
/// One inbound scope can bind one resolved employee/turn/session and cannot
/// be retargeted later. An adapter scope alone never authorizes a model turn.
pub(crate) fn bind_final_route(
    conversation: &str,
    final_session: &str,
    agent: &str,
    turn: &str,
) -> bool {
    let Some(proof) = VERIFIED_INPUT.try_with(Clone::clone).ok().flatten() else {
        return false;
    };
    if !proof.live.load(Ordering::Acquire)
        || proof.session != conversation
        || !duduclaw_core::is_valid_agent_id(agent)
        || !super::turns::valid_turn_id(turn)
        || final_session.is_empty()
        || final_session.len() > 512
        || final_session.chars().any(char::is_control)
    {
        return false;
    }
    if final_session != conversation
        && final_session != crate::trigger_route::called_session(conversation, agent)
    {
        return false;
    }
    // Same leaf-lock order as capture/valid_for; never held across await.
    let bound = proof.bound.lock().unwrap_or_else(|p| p.into_inner());
    if bound.as_ref().is_some_and(|(a, t)| a != agent || t != turn) {
        return false;
    }
    let candidate = FinalRoute {
        session: final_session.into(),
        agent: agent.into(),
        turn: turn.into(),
    };
    let mut route = proof.final_route.lock().unwrap_or_else(|p| p.into_inner());
    match route.as_ref() {
        Some(existing) => existing == &candidate,
        None => {
            *route = Some(candidate);
            true
        }
    }
}

pub(crate) fn capture(agent: &str, turn: &str) -> Option<AttendedInputProof> {
    let session = duduclaw_memory::feedback::CURRENT_SESSION_ID
        .try_with(Clone::clone)
        .ok()
        .flatten()?;
    let proof = VERIFIED_INPUT.try_with(Clone::clone).ok().flatten()?;
    if !proof.live.load(Ordering::Acquire) {
        return None;
    }
    {
        let mut bound = proof.bound.lock().unwrap_or_else(|p| p.into_inner());
        let route = proof.final_route.lock().unwrap_or_else(|p| p.into_inner());
        let route = route.as_ref()?;
        if route.session != session || route.agent != agent || route.turn != turn {
            return None;
        }
        match bound.as_ref() {
            Some((a, t)) if a != agent || t != turn => return None,
            None => *bound = Some((agent.into(), turn.into())),
            _ => {}
        }
    }
    Some(proof)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn actual_routed_turn(
        conversation: &str,
        proof_conversation: &str,
        own_bot: bool,
    ) -> crate::agent_channels::e2e_seam::AttendanceObservation {
        let home = tempfile::tempdir().unwrap();
        for (id, kind) in [("dudu", "main"), ("sales", "specialist")] {
            crate::trigger_route::tests::write_agent(
                &home.path().join("agents"),
                id,
                id,
                &format!("@{id}"),
                kind,
                "",
            );
        }
        let ctx = crate::agent_channels::e2e_tests::reply_ctx(home.path()).await;
        crate::agent_channels::e2e_seam::arm(home.path());
        verified_channel_message(proof_conversation, "verified-human", false, async {
            let work = crate::channel_reply::build_guarded_reply_for_agent(
                "synthetic routing task",
                &ctx,
                "sales",
                conversation,
                "verified-human",
                None,
            );
            let reply = if own_bot {
                crate::trigger_route::as_own_bot_turn(work).await
            } else {
                crate::trigger_route::as_called_turn(true, work).await
            };
            assert!(reply.text.contains("OWN_BOT_REPLY"), "{}", reply.text);
        })
        .await;
        let mut observed = crate::agent_channels::e2e_seam::take_attendance(home.path());
        assert_eq!(observed.len(), 1);
        observed.pop().unwrap()
    }

    #[tokio::test]
    async fn attended_foundation_actual_called_and_own_bot_routes_register_final_session() {
        for (conversation, own_bot) in [
            ("line:attendance-called", false),
            ("telegram:attendance-own", true),
        ] {
            let observed = actual_routed_turn(conversation, conversation, own_bot).await;
            assert_eq!(observed.agent, "sales");
            assert_eq!(
                observed.session,
                crate::trigger_route::called_session(conversation, "sales")
            );
            assert!(observed.attended, "{}", observed.session);
        }
    }

    #[tokio::test]
    async fn attended_foundation_actual_reroute_never_borrows_another_conversation() {
        for own_bot in [false, true] {
            let observed = actual_routed_turn(
                "telegram:attendance-other",
                "telegram:attendance-source",
                own_bot,
            )
            .await;
            assert_eq!(observed.session, "telegram:attendance-other#agent:sales");
            assert!(!observed.attended);
        }
    }
    #[tokio::test]
    async fn human_input_is_bound_to_live_session_agent_and_turn_and_drops_with_scope() {
        let proof = verified_webchat_message(
            "webchat:synthetic",
            Some("alice"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(
                Some("webchat:synthetic".into()),
                async {
                    assert!(bind_final_route(
                        "webchat:synthetic",
                        "webchat:synthetic",
                        "employee-a",
                        "turn-a"
                    ));
                    let proof = capture("employee-a", "turn-a").unwrap();
                    assert!(proof.valid_for("employee-a", "turn-a"));
                    assert!(capture("employee-b", "turn-a").is_none());
                    assert!(capture("employee-a", "turn-b").is_none());
                    let child = tokio::spawn(async { capture("employee-a", "turn-a") });
                    assert!(child.await.unwrap().is_none());
                    proof
                },
            ),
        )
        .await;
        assert!(!proof.valid_for("employee-a", "turn-a"));
    }
    #[tokio::test]
    async fn internal_bot_public_unknown_and_cross_session_do_not_inherit_attendance() {
        for (session, sender, bot) in [
            ("goal:x", "alice", false),
            ("heartbeat:x", "alice", false),
            ("workflow:x", "alice", false),
            ("dispatch:x", "alice", false),
            ("unknown:x", "alice", false),
            ("telegram:1", "system", false),
            ("telegram:1", "alice", true),
            ("odoolivechat:1", "alice", false),
        ] {
            verified_channel_message(
                session,
                sender,
                bot,
                duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.into()), async {
                    assert!(capture("employee-a", "turn-a").is_none(), "{session}");
                }),
            )
            .await;
        }
        verified_webchat_message(
            "webchat:visitor",
            Some("widget-visitor:1"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(
                Some("webchat:visitor".into()),
                async {
                    assert!(capture("employee-a", "turn-a").is_none());
                },
            ),
        )
        .await;
        verified_webchat_message(
            "webchat:human",
            Some("alice"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(
                Some("dispatch:other".into()),
                async {
                    assert!(bind_final_route(
                        "webchat:human",
                        "webchat:human",
                        "employee-a",
                        "turn-a"
                    ));
                    assert!(capture("employee-a", "turn-a").is_none());
                },
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn live_turn_registry_requires_proof_and_scope_completion_revokes_it() {
        let agent = "attended-registry-alice";
        let turn = "attended-registry-turn";
        let session = "telegram:attended-registry";
        let guard = verified_channel_message(
            session,
            "verified-human",
            false,
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(
                Some(session.into()),
                duduclaw_memory::feedback::CURRENT_TURN_ID.scope(
                    Some(turn.into()),
                    crate::claude_runner::REPLY_CHANNEL.scope(session.into(), async {
                        assert!(bind_final_route(session, session, agent, turn));
                        let guard = super::super::turns::register_current_turn(agent).unwrap();
                        assert!(super::super::turns::attended_live_for(agent, turn));
                        assert!(!super::super::turns::attended_live_for(
                            "attended-registry-other",
                            turn
                        ));
                        assert!(!super::super::turns::attended_live_for(
                            agent,
                            "another-turn"
                        ));
                        guard
                    }),
                ),
            ),
        )
        .await;
        // The registration deliberately outlives the verified inbound handler.
        assert!(!super::super::turns::attended_live_for(agent, turn));
        drop(guard);
        let untrusted = super::super::turns::register(agent, turn, session).unwrap();
        assert!(!super::super::turns::attended_live_for(agent, turn));
        drop(untrusted);
    }

    #[tokio::test]
    async fn canceling_the_inbound_handler_revokes_a_turn_even_if_its_registration_is_retained() {
        let (send, receive) = tokio::sync::oneshot::channel();
        let agent = "attended-cancel-alice";
        let turn = "attended-cancel-turn";
        let task = tokio::spawn(verified_webchat_message(
            "webchat:attended-cancel",
            Some("alice"),
            duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(
                Some("webchat:attended-cancel".into()),
                async move {
                    assert!(bind_final_route(
                        "webchat:attended-cancel",
                        "webchat:attended-cancel",
                        agent,
                        turn
                    ));
                    let guard =
                        super::super::turns::register(agent, turn, "webchat:attended-cancel")
                            .unwrap();
                    send.send(guard).unwrap();
                    std::future::pending::<()>().await;
                },
            ),
        ));
        let guard = receive.await.unwrap();
        assert!(super::super::turns::attended_live_for(agent, turn));
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!super::super::turns::attended_live_for(agent, turn));
        drop(guard);
    }

    #[tokio::test]
    async fn attended_foundation_final_route_is_exact_immutable_and_required() {
        let base = "telegram:attendance-exact";
        let final_session = "telegram:attendance-exact#agent:sales";
        verified_channel_message(base, "verified-human", false, async {
            duduclaw_memory::feedback::CURRENT_SESSION_ID
                .scope(Some(base.into()), async {
                    assert!(
                        capture("sales", "exact-turn").is_none(),
                        "unrouted input is not a model turn"
                    );
                })
                .await;
            assert!(!bind_final_route(
                "telegram:other",
                final_session,
                "sales",
                "exact-turn"
            ));
            assert!(bind_final_route(base, final_session, "sales", "exact-turn"));
            assert!(bind_final_route(base, final_session, "sales", "exact-turn"));
            assert!(!bind_final_route(
                base,
                "telegram:other#agent:sales",
                "sales",
                "exact-turn"
            ));
            assert!(!bind_final_route(
                base,
                final_session,
                "other",
                "exact-turn"
            ));
            for wrong in [
                base,
                "telegram:attendance-exact#agent:sales-extra",
                "telegram:attendance-exact#agent:sales#agent:other",
            ] {
                duduclaw_memory::feedback::CURRENT_SESSION_ID
                    .scope(Some(wrong.into()), async {
                        assert!(capture("sales", "exact-turn").is_none(), "{wrong}");
                    })
                    .await;
            }
            duduclaw_memory::feedback::CURRENT_SESSION_ID
                .scope(Some(final_session.into()), async {
                    assert!(capture("sales", "exact-turn").is_some());
                    assert!(capture("sales", "other-turn").is_none());
                })
                .await;
        })
        .await;
    }
}
