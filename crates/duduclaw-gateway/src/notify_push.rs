//! `push(card, dest)` — the one outbound entry for every decision card.
//!
//! # Why this module exists (O5, 2026-09-29)
//!
//! Four notification modules — [`crate::goal_notify`],
//! [`crate::approval_notify`], [`crate::install_notify`],
//! [`crate::autopilot_notify`] — each wrote their own version of the same
//! outbound loop, and said so in their own doc comments ("mirroring
//! `install_notify.rs`", "the same situation `autopilot_notify` is in").
//! Reading the four side by side, only two things actually differed:
//!
//! 1. **where** the card goes (an agent's `[proactive]` channel / a linked
//!    dashboard user's DM / a broker-recorded destination), and
//! 2. **what** the card says.
//!
//! Everything after that was the same four steps written four ways: resolve a
//! bot token, hand the card to [`crate::decision_notify::deliver_outcome`],
//! remember the first destination that worked, and keep going. That loop is
//! now here, once.
//!
//! # What this module deliberately does NOT own
//!
//! - **Authorization.** Who may press a button is
//!   [`crate::decision_notify::authorize_press`] and each module's own
//!   `delivered_targets`; not one line of it moved. A push is an outbound
//!   side effect — it must never be able to widen who can decide.
//! - **Card rendering.** Bodies, deep links and the no-button hint stay with
//!   the source that knows the domain (a goal has three buttons, an install
//!   has a sign-off, an autopilot card reports a breaker state).
//! - **Quiet hours / deferral.** Still [`crate::decision_notify`]'s
//!   `deliver_outcome`, which reads the level from `card.source`.
//! - **Takeover deferral.** Checked by the caller *before* the card is even
//!   rendered (a held-back card must not pay for an LLM trajectory call).
//!
//! # Token dialects
//!
//! The two [`NotifyDest`] variants exist because the platform genuinely has
//! two token resolutions, and picking the wrong one is a 401:
//!
//! | dest | token source | delivery |
//! |------|--------------|----------|
//! | [`NotifyDest::Agent`] | the agent's own `[channels.<ch>]`, then the `reports_to` cascade, then global (`goal_notify::channel_token`) | every target attempted |
//! | [`NotifyDest::LinkedUsers`] | the global DM candidate list (`config_crypto::channel_dm_token_candidates`) | every target attempted; per target, candidates tried until one send succeeds |

use std::path::Path;

use tracing::info;

use crate::decision_notify::{DecisionCard, DeliverOutcome};

/// The card to push. An alias rather than a new struct: [`DecisionCard`] is
/// already the shape every notifier builds and the shape
/// `decision_notify::deliver_outcome` consumes, and a parallel type would be
/// a fifth copy of the thing this module exists to remove.
pub(crate) type NotifyCard<'a> = DecisionCard<'a>;

/// Where a card goes, and therefore which token dialect reaches it.
pub(crate) enum NotifyDest {
    /// Destinations reachable with **the agent's own** bot token cascade.
    ///
    /// Used by goal cards (the assigned agent's `[proactive]` channel),
    /// autopilot circuit-open cards (the rule's target agent), and approval
    /// cards (whose chain already resolved to `(channel, chat_id)` pairs).
    Agent {
        agent_id: String,
        targets: Vec<(String, String)>,
    },
    /// Destinations that are **linked dashboard-user identities**, reached
    /// with the deployment's DM bot tokens.
    ///
    /// Used by install sign-off cards: the approver is a person, not an
    /// agent, so there is no per-agent token to cascade from. Every candidate
    /// token is tried because a deployment can run several bots on one
    /// channel and only one of them shares a conversation with this person.
    LinkedUsers { targets: Vec<(String, String)> },
}

impl NotifyDest {
    /// A single agent-scoped destination — the common case.
    pub(crate) fn agent(agent_id: impl Into<String>, channel: String, chat_id: String) -> Self {
        Self::Agent {
            agent_id: agent_id.into(),
            targets: vec![(channel, chat_id)],
        }
    }

    fn targets(&self) -> &[(String, String)] {
        match self {
            Self::Agent { targets, .. } | Self::LinkedUsers { targets } => targets,
        }
    }
}

/// What [`push`] achieved.
pub(crate) struct Receipt {
    /// The **first** destination the card reached — `Some` when that target's
    /// outcome was anything but [`DeliverOutcome::Failed`], i.e. a queued
    /// quiet-hours card counts as reached (a retry would duplicate it). This
    /// is the value the `ApprovalBroker` persists so the reminder and the
    /// inbound press follow the same conversation.
    pub(crate) delivered: Option<(String, String)>,
    /// Aggregate outcome: `Sent` if any target was sent now, else `Deferred`
    /// if any was queued, else `Failed`. With a single target — every caller
    /// that reads this one — it is exactly that target's outcome.
    pub(crate) outcome: DeliverOutcome,
    /// Targets that had a usable token and were actually attempted. `0` with
    /// a non-empty `dest` means "nothing was reachable", which callers report
    /// as no-target rather than as a send failure.
    pub(crate) attempted: usize,
}

impl Receipt {
    fn empty() -> Self {
        Self {
            delivered: None,
            outcome: DeliverOutcome::Failed,
            attempted: 0,
        }
    }
}

/// Push one card to one destination set.
///
/// Best-effort throughout: a missing token, an unconfigured destination or a
/// platform error is logged and skipped, never panics, and never aborts the
/// remaining targets. The caller decides what an empty [`Receipt`] means for
/// its own domain.
pub(crate) async fn push(
    home_dir: &Path,
    card: &NotifyCard<'_>,
    dest: &NotifyDest,
) -> Receipt {
    if dest.targets().is_empty() {
        return Receipt::empty();
    }
    let http = reqwest::Client::new();
    let mut receipt = Receipt::empty();

    for (channel, chat_id) in dest.targets() {
        let outcome = match dest {
            NotifyDest::Agent { agent_id, .. } => {
                let Some(token) = crate::goal_notify::channel_token(home_dir, agent_id, channel)
                    .await
                else {
                    info!(
                        decision = %card.decision_id, %channel,
                        "notify-push: no bot token for agent destination; skipping"
                    );
                    continue;
                };
                receipt.attempted += 1;
                crate::decision_notify::deliver_outcome(
                    home_dir, &http, channel, &token, chat_id, card,
                )
                .await
            }
            NotifyDest::LinkedUsers { .. } => {
                let candidates =
                    crate::config_crypto::channel_dm_token_candidates(home_dir, channel).await;
                if candidates.is_empty() {
                    info!(
                        decision = %card.decision_id, %channel,
                        "notify-push: no DM bot token configured; skipping"
                    );
                    continue;
                }
                receipt.attempted += 1;
                let mut best = DeliverOutcome::Failed;
                for token in &candidates {
                    best = crate::decision_notify::deliver_outcome(
                        home_dir, &http, channel, token, chat_id, card,
                    )
                    .await;
                    if best != DeliverOutcome::Failed {
                        break;
                    }
                }
                best
            }
        };

        if outcome != DeliverOutcome::Failed && receipt.delivered.is_none() {
            receipt.delivered = Some((channel.clone(), chat_id.clone()));
        }
        receipt.outcome = merge(receipt.outcome, outcome);
    }
    receipt
}

/// `Sent` beats `Deferred` beats `Failed` — a card that reached one live
/// destination is delivered even if another target's token was dead.
fn merge(acc: DeliverOutcome, next: DeliverOutcome) -> DeliverOutcome {
    match (acc, next) {
        (DeliverOutcome::Sent, _) | (_, DeliverOutcome::Sent) => DeliverOutcome::Sent,
        (DeliverOutcome::Deferred, _) | (_, DeliverOutcome::Deferred) => DeliverOutcome::Deferred,
        _ => DeliverOutcome::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_prefers_sent_then_deferred() {
        use DeliverOutcome::*;
        assert_eq!(merge(Failed, Sent), Sent);
        assert_eq!(merge(Sent, Failed), Sent);
        assert_eq!(merge(Deferred, Sent), Sent);
        assert_eq!(merge(Failed, Deferred), Deferred);
        assert_eq!(merge(Deferred, Failed), Deferred);
        assert_eq!(merge(Failed, Failed), Failed);
    }

    /// A single-target push must report exactly that target's outcome — the
    /// property `goal_notify` relies on to map a `Receipt` onto its
    /// three-state `NotifyOutcome`.
    #[test]
    fn single_target_outcome_is_the_targets_outcome() {
        use DeliverOutcome::*;
        for o in [Sent, Deferred, Failed] {
            assert_eq!(merge(Receipt::empty().outcome, o), o);
        }
    }

    #[test]
    fn empty_destination_is_no_target_not_a_failure() {
        let r = Receipt::empty();
        assert!(r.delivered.is_none());
        assert_eq!(r.attempted, 0);
    }

    #[test]
    fn agent_helper_builds_one_target() {
        let d = NotifyDest::agent("dudu", "telegram".into(), "42".into());
        assert_eq!(d.targets(), &[("telegram".to_string(), "42".to_string())]);
        match d {
            NotifyDest::Agent { agent_id, .. } => assert_eq!(agent_id, "dudu"),
            _ => panic!("expected Agent"),
        }
    }

    #[tokio::test]
    async fn push_with_no_targets_never_touches_the_network() {
        let home = tempfile::tempdir().unwrap();
        let card = NotifyCard {
            source: crate::decision_action::DecisionSource::Goal,
            decision_id: "t1",
            body: "body",
            link: None,
            no_button_hint: "hint",
        };
        let r = push(
            home.path(),
            &card,
            &NotifyDest::Agent {
                agent_id: "dudu".into(),
                targets: vec![],
            },
        )
        .await;
        assert_eq!(r.attempted, 0);
        assert!(r.delivered.is_none());
        assert_eq!(r.outcome, DeliverOutcome::Failed);
    }
}
