//! One wire format for every "a human must decide this" button, across every
//! channel and every backing store.
//!
//! ## Why one codec
//!
//! Five independent encodings grew up alongside five HITL entry points
//! (install two-stage sign-off, goal-loop needs_human, goal-loop kickoff,
//! the generic `ApprovalBroker`, autopilot circuit-breaker pause). Each had
//! its own `strip_prefix` parser and its own inbound branch in all four
//! channel dispatchers, so adding a sixth meant touching nine files and
//! every dispatcher grew another near-identical block. This module is the
//! single encode/decode point; [`DecisionSource`] is the only thing that
//! differs between them, and it is an internal attribute — never shown to a
//! user.
//!
//! ## Wire format
//!
//! ```text
//! duduclaw:decide:<source>:<verb>:<id>
//! ```
//!
//! `<id>` is the owning store's primary key, kept verbatim — everything after
//! the verb separator is the id, so an id containing `:` survives a round
//! trip. Legal `(source, verb)` pairs are enumerated in [`DecisionAct::valid_for`];
//! anything else fails closed to `None` rather than being guessed at.
//!
//! ## Why short source/verb tokens
//!
//! Telegram caps `callback_data` at 64 bytes — a hard platform limit, and
//! exceeding it means the button silently fails to send. Every decision id in
//! this codebase is a v4 UUID (36 chars), so the prefix budget is 28 bytes.
//! `duduclaw:decide:autopilot:pause:` would be 32 (over); the short tokens
//! used here cap the prefix at 27, leaving one byte of headroom.
//! [`tests::longest_encoding_fits_telegram_callback_limit`] pins that
//! invariant so a future longer id fails a test instead of failing silently
//! on a user's phone.
//!
//! ## Backward compatibility
//!
//! Cards already delivered to a channel carry the pre-unification encodings
//! and stay clickable for as long as the platform keeps the message around
//! (install sign-offs have no TTL at all). [`parse`] therefore tries the
//! unified format first and then the five legacy prefixes, mapping each onto
//! the same [`DecisionAction`] — the legacy encodings are *translated*, not
//! given their own code path. Only [`encode`] is single-format: nothing new
//! is ever emitted in a legacy shape.

/// Which HITL store a decision belongs to. An internal routing attribute —
/// user-facing copy describes what the decision *is about*, never which
/// subsystem filed it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecisionSource {
    /// A goal-loop task parked awaiting a human (`TaskStore`).
    Goal,
    /// A goal-loop kickoff gate — an `ApprovalBroker` row that must clear
    /// before the first autonomous dispatch.
    Kickoff,
    /// The generic `ApprovalBroker` (`approvals.db`).
    Approval,
    /// A two-stage install sign-off (`install_requests`).
    Install,
    /// An autopilot rule whose circuit breaker tripped (`autopilot.db`).
    Autopilot,
}

impl DecisionSource {
    /// The short wire token.
    pub fn token(self) -> &'static str {
        match self {
            Self::Goal => "goal",
            Self::Kickoff => "kick",
            Self::Approval => "apv",
            Self::Install => "inst",
            Self::Autopilot => "auto",
        }
    }

    pub(crate) fn from_token(s: &str) -> Option<Self> {
        match s {
            "goal" => Some(Self::Goal),
            "kick" => Some(Self::Kickoff),
            "apv" => Some(Self::Approval),
            "inst" => Some(Self::Install),
            "auto" => Some(Self::Autopilot),
            _ => None,
        }
    }

    /// The `decision_message_store` namespace this source's cards are filed
    /// under. These strings are load-bearing across an upgrade: entries
    /// written by an older build are keyed by them, so they must never be
    /// "tidied up" to match the wire tokens.
    pub fn namespace(self) -> &'static str {
        match self {
            Self::Goal => "goal_task",
            Self::Kickoff => "goal_kickoff",
            Self::Approval => "approval",
            Self::Install => "install",
            Self::Autopilot => "autopilot_circuit",
        }
    }
}

/// What the presser chose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecisionAct {
    /// Goal-loop: send the task back for another attempt.
    Retry,
    /// Goal-loop: accept the current result as finished.
    Done,
    /// Goal-loop: give up on the task.
    Abort,
    /// Approve / allow / sign off.
    Approve,
    /// Refuse.
    Deny,
    /// Autopilot: disable the rule.
    Pause,
    /// Goal-loop: a human takes over a `needs_human` task by hand instead of
    /// approving one of the automated exits (D6, Intercom `Loop in teammate`
    /// `Submit`/`Take over`). Deliberately distinct from [`Self::Done`]/
    /// [`Self::Abort`] — it does not resolve the task, only claims it.
    Takeover,
}

impl DecisionAct {
    /// The short wire token.
    pub fn token(self) -> &'static str {
        match self {
            Self::Retry => "retry",
            Self::Done => "done",
            Self::Abort => "abort",
            Self::Approve => "ok",
            Self::Deny => "no",
            Self::Pause => "pause",
            Self::Takeover => "take",
        }
    }

    pub(crate) fn from_token(s: &str) -> Option<Self> {
        match s {
            "retry" => Some(Self::Retry),
            "done" => Some(Self::Done),
            "abort" => Some(Self::Abort),
            "ok" => Some(Self::Approve),
            "no" => Some(Self::Deny),
            "pause" => Some(Self::Pause),
            "take" => Some(Self::Takeover),
            _ => None,
        }
    }

    /// Whether this act is meaningful for `source`. Enumerated explicitly so
    /// an unlisted combination denies rather than falling through to a
    /// handler that would have to guess (project convention: security and
    /// routing gates fail closed).
    pub fn valid_for(self, source: DecisionSource) -> bool {
        match source {
            DecisionSource::Goal => {
                matches!(self, Self::Retry | Self::Done | Self::Abort | Self::Takeover)
            }
            DecisionSource::Kickoff | DecisionSource::Approval | DecisionSource::Install => {
                matches!(self, Self::Approve | Self::Deny)
            }
            DecisionSource::Autopilot => matches!(self, Self::Pause),
        }
    }
}

/// A decoded button press.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionAction {
    pub source: DecisionSource,
    pub act: DecisionAct,
    /// The owning store's primary key, verbatim.
    pub id: String,
}

impl DecisionAction {
    /// `true` when [`DecisionAct::Approve`], for the many call sites that
    /// take a plain `approve: bool`.
    pub fn approve(&self) -> bool {
        self.act == DecisionAct::Approve
    }
}

/// Shared prefix of the unified format.
const PREFIX: &str = "duduclaw:decide:";

/// Telegram's `callback_data` byte cap — the tightest of the four
/// button-capable platforms (Discord allows 100, Slack's `action_id` 255,
/// LINE's postback `data` 300).
pub const MAX_ACTION_ID_BYTES: usize = 64;

/// Build the wire id for a decision button. The `(source, act)` pair is
/// assumed valid at the call site (button builders are the only callers and
/// each hard-codes its own pair); [`parse`] re-validates on the way in, which
/// is where an untrusted value could actually appear.
pub fn encode(source: DecisionSource, act: DecisionAct, id: &str) -> String {
    format!("{PREFIX}{}:{}:{id}", source.token(), act.token())
}

/// Decode a button press. Tries the unified format, then the five legacy
/// encodings. `None` for anything not a well-formed decision action — an
/// empty id, an unknown source/verb, or an illegal `(source, verb)` pair all
/// fail closed rather than being interpreted.
pub fn parse(data: &str) -> Option<DecisionAction> {
    parse_unified(data).or_else(|| parse_legacy(data))
}

fn parse_unified(data: &str) -> Option<DecisionAction> {
    let rest = data.strip_prefix(PREFIX)?;
    // source and verb are fixed vocabularies with no `:`; everything after
    // the second separator is the id, verbatim.
    let (source_tok, rest) = rest.split_once(':')?;
    let (act_tok, id) = rest.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    let source = DecisionSource::from_token(source_tok)?;
    let act = DecisionAct::from_token(act_tok)?;
    if !act.valid_for(source) {
        return None;
    }
    Some(DecisionAction { source, act, id: id.to_string() })
}

/// The pre-unification encodings, kept parseable so cards already sitting in
/// a channel stay clickable. Ordered longest-prefix-first is unnecessary
/// here — the five prefixes are mutually exclusive.
fn parse_legacy(data: &str) -> Option<DecisionAction> {
    const LEGACY: &[(&str, DecisionSource, DecisionAct)] = &[
        ("duduclaw:install_approve:", DecisionSource::Install, DecisionAct::Approve),
        ("duduclaw:install_deny:", DecisionSource::Install, DecisionAct::Deny),
        ("duduclaw:goal_retry:", DecisionSource::Goal, DecisionAct::Retry),
        ("duduclaw:goal_done:", DecisionSource::Goal, DecisionAct::Done),
        ("duduclaw:goal_abort:", DecisionSource::Goal, DecisionAct::Abort),
        ("duduclaw:goal_kickoff_ok:", DecisionSource::Kickoff, DecisionAct::Approve),
        ("duduclaw:goal_kickoff_no:", DecisionSource::Kickoff, DecisionAct::Deny),
        ("duduclaw:approval_ok:", DecisionSource::Approval, DecisionAct::Approve),
        ("duduclaw:approval_no:", DecisionSource::Approval, DecisionAct::Deny),
        ("duduclaw:autopilot_pause:", DecisionSource::Autopilot, DecisionAct::Pause),
    ];
    for (prefix, source, act) in LEGACY {
        if let Some(id) = data.strip_prefix(prefix) {
            if id.is_empty() {
                return None;
            }
            return Some(DecisionAction { source: *source, act: *act, id: id.to_string() });
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL_SOURCES: [DecisionSource; 5] = [
        DecisionSource::Goal,
        DecisionSource::Kickoff,
        DecisionSource::Approval,
        DecisionSource::Install,
        DecisionSource::Autopilot,
    ];
    const ALL_ACTS: [DecisionAct; 7] = [
        DecisionAct::Retry,
        DecisionAct::Done,
        DecisionAct::Abort,
        DecisionAct::Approve,
        DecisionAct::Deny,
        DecisionAct::Pause,
        DecisionAct::Takeover,
    ];

    #[test]
    fn round_trips_every_legal_pair() {
        for source in ALL_SOURCES {
            for act in ALL_ACTS {
                if !act.valid_for(source) {
                    continue;
                }
                let wire = encode(source, act, "abc-123");
                let got = parse(&wire).expect("legal pair must decode");
                assert_eq!(got.source, source);
                assert_eq!(got.act, act);
                assert_eq!(got.id, "abc-123");
            }
        }
    }

    #[test]
    fn illegal_source_verb_pairs_fail_closed() {
        // A goal task cannot be "paused"; an autopilot rule cannot be
        // "retried". Encoding them is possible (the builders never do), but
        // decoding must refuse.
        assert_eq!(parse(&encode(DecisionSource::Goal, DecisionAct::Pause, "x")), None);
        assert_eq!(parse(&encode(DecisionSource::Autopilot, DecisionAct::Retry, "x")), None);
        assert_eq!(parse(&encode(DecisionSource::Approval, DecisionAct::Done, "x")), None);
        assert_eq!(parse(&encode(DecisionSource::Install, DecisionAct::Abort, "x")), None);
        // W1-5: "take over" only makes sense for a goal-loop needs_human task.
        assert_eq!(parse(&encode(DecisionSource::Kickoff, DecisionAct::Takeover, "x")), None);
        assert_eq!(parse(&encode(DecisionSource::Approval, DecisionAct::Takeover, "x")), None);
        assert_eq!(parse(&encode(DecisionSource::Install, DecisionAct::Takeover, "x")), None);
        assert_eq!(parse(&encode(DecisionSource::Autopilot, DecisionAct::Takeover, "x")), None);
    }

    #[test]
    fn goal_takeover_round_trips() {
        let wire = encode(DecisionSource::Goal, DecisionAct::Takeover, "g-1");
        assert_eq!(wire, "duduclaw:decide:goal:take:g-1");
        let got = parse(&wire).unwrap();
        assert_eq!(got.source, DecisionSource::Goal);
        assert_eq!(got.act, DecisionAct::Takeover);
        assert_eq!(got.id, "g-1");
        assert!(!got.approve(), "takeover is not an approve/deny act");
    }

    #[test]
    fn malformed_input_fails_closed() {
        for data in [
            "",
            "garbage",
            "duduclaw:new_session",
            "duduclaw:decide:",
            "duduclaw:decide:apv",
            "duduclaw:decide:apv:ok",     // no id separator
            "duduclaw:decide:apv:ok:",    // empty id
            "duduclaw:decide:nope:ok:x",  // unknown source
            "duduclaw:decide:apv:maybe:x", // unknown verb
            "duduclaw:decide:APV:ok:x",   // case-sensitive vocabulary
            "decide:apv:ok:x",            // missing namespace
        ] {
            assert_eq!(parse(data), None, "must refuse {data:?}");
        }
    }

    #[test]
    fn id_keeps_colons_verbatim() {
        // Nothing generates such an id today, but the parser must not
        // truncate one — a silently-shortened id would decide the wrong row.
        let wire = encode(DecisionSource::Approval, DecisionAct::Approve, "ns:sub:id");
        assert_eq!(parse(&wire).unwrap().id, "ns:sub:id");
    }

    // ── backward compatibility: cards already sitting in a channel ──

    #[test]
    fn legacy_install_encoding_still_parses() {
        assert_eq!(
            parse("duduclaw:install_approve:r-1"),
            Some(DecisionAction { source: DecisionSource::Install, act: DecisionAct::Approve, id: "r-1".into() })
        );
        assert_eq!(
            parse("duduclaw:install_deny:r-1"),
            Some(DecisionAction { source: DecisionSource::Install, act: DecisionAct::Deny, id: "r-1".into() })
        );
    }

    #[test]
    fn legacy_goal_encoding_still_parses() {
        assert_eq!(
            parse("duduclaw:goal_retry:t-1"),
            Some(DecisionAction { source: DecisionSource::Goal, act: DecisionAct::Retry, id: "t-1".into() })
        );
        assert_eq!(
            parse("duduclaw:goal_done:t-1"),
            Some(DecisionAction { source: DecisionSource::Goal, act: DecisionAct::Done, id: "t-1".into() })
        );
        assert_eq!(
            parse("duduclaw:goal_abort:t-1"),
            Some(DecisionAction { source: DecisionSource::Goal, act: DecisionAct::Abort, id: "t-1".into() })
        );
    }

    #[test]
    fn legacy_kickoff_encoding_still_parses() {
        assert_eq!(
            parse("duduclaw:goal_kickoff_ok:ap-9"),
            Some(DecisionAction { source: DecisionSource::Kickoff, act: DecisionAct::Approve, id: "ap-9".into() })
        );
        assert_eq!(
            parse("duduclaw:goal_kickoff_no:ap-9"),
            Some(DecisionAction { source: DecisionSource::Kickoff, act: DecisionAct::Deny, id: "ap-9".into() })
        );
    }

    #[test]
    fn legacy_broker_encoding_still_parses() {
        assert_eq!(
            parse("duduclaw:approval_ok:ap-1"),
            Some(DecisionAction { source: DecisionSource::Approval, act: DecisionAct::Approve, id: "ap-1".into() })
        );
        assert_eq!(
            parse("duduclaw:approval_no:ap-1"),
            Some(DecisionAction { source: DecisionSource::Approval, act: DecisionAct::Deny, id: "ap-1".into() })
        );
    }

    #[test]
    fn legacy_autopilot_encoding_still_parses() {
        assert_eq!(
            parse("duduclaw:autopilot_pause:r-1"),
            Some(DecisionAction { source: DecisionSource::Autopilot, act: DecisionAct::Pause, id: "r-1".into() })
        );
    }

    #[test]
    fn legacy_and_unified_encodings_decode_identically() {
        let pairs = [
            ("duduclaw:install_approve:x", DecisionSource::Install, DecisionAct::Approve),
            ("duduclaw:goal_abort:x", DecisionSource::Goal, DecisionAct::Abort),
            ("duduclaw:goal_kickoff_no:x", DecisionSource::Kickoff, DecisionAct::Deny),
            ("duduclaw:approval_ok:x", DecisionSource::Approval, DecisionAct::Approve),
            ("duduclaw:autopilot_pause:x", DecisionSource::Autopilot, DecisionAct::Pause),
        ];
        for (legacy, source, act) in pairs {
            assert_eq!(parse(legacy), parse(&encode(source, act, "x")), "legacy {legacy} must be equivalent");
        }
    }

    #[test]
    fn legacy_encodings_with_empty_id_fail_closed() {
        for data in [
            "duduclaw:install_approve:",
            "duduclaw:goal_retry:",
            "duduclaw:goal_kickoff_ok:",
            "duduclaw:approval_no:",
            "duduclaw:autopilot_pause:",
        ] {
            assert_eq!(parse(data), None, "must refuse id-less {data:?}");
        }
    }

    // ── platform limits ──

    #[test]
    fn longest_encoding_fits_telegram_callback_limit() {
        // Every decision id in this codebase is a v4 UUID. If that ever
        // changes, this test — not a user's silently-undelivered button — is
        // where it surfaces.
        let uuid = uuid::Uuid::new_v4().to_string();
        assert_eq!(uuid.len(), 36);
        for source in ALL_SOURCES {
            for act in ALL_ACTS {
                if !act.valid_for(source) {
                    continue;
                }
                let wire = encode(source, act, &uuid);
                assert!(
                    wire.len() <= MAX_ACTION_ID_BYTES,
                    "{wire} is {} bytes, over Telegram's {MAX_ACTION_ID_BYTES}",
                    wire.len()
                );
            }
        }
    }

    #[test]
    fn namespaces_are_stable_and_distinct() {
        // These strings key on-disk state written by earlier builds; renaming
        // one would orphan every in-flight card's stored position.
        assert_eq!(DecisionSource::Goal.namespace(), "goal_task");
        assert_eq!(DecisionSource::Kickoff.namespace(), "goal_kickoff");
        assert_eq!(DecisionSource::Approval.namespace(), "approval");
        assert_eq!(DecisionSource::Install.namespace(), "install");
        assert_eq!(DecisionSource::Autopilot.namespace(), "autopilot_circuit");
        let mut seen: Vec<&str> = ALL_SOURCES.iter().map(|s| s.namespace()).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), ALL_SOURCES.len(), "namespaces must not collide");
    }

    #[test]
    fn approve_helper_matches_the_act() {
        let a = DecisionAction { source: DecisionSource::Approval, act: DecisionAct::Approve, id: "x".into() };
        let d = DecisionAction { source: DecisionSource::Approval, act: DecisionAct::Deny, id: "x".into() };
        assert!(a.approve());
        assert!(!d.approve());
    }
}
