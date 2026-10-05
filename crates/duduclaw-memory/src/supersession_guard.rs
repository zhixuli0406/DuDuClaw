//! Supersession trust guard (WP1 follow-up, 2026-10).
//!
//! F1 temporal memory lets a new fact with the same `(agent, subject,
//! predicate)` replace the currently valid one. WP1 bound every write to an
//! origin with a trust ceiling, but nothing compared the two trusts at the
//! moment of supersession: a `channel` write (ceiling 0.3) — i.e. anything a
//! chat participant can get distilled — replaced whatever was current for the
//! triple regardless of its trust (in practice an agent-derived or
//! unattributed 0.6 fact; no production path wrote operator-origin facts
//! before this guard, whose review promotion is the first). That is the
//! memory-poisoning primitive the origin table exists to stop.
//!
//! The rule enforced in [`SqliteMemoryEngine::store_temporal_outcome`]
//! (`crate::engine`): **a write may not supersede a currently valid fact whose
//! origin trust is strictly higher than the write's own effective trust.**
//! Equal or higher trust supersedes exactly as before. A refusal writes
//! nothing and is returned to the caller as [`TemporalWriteOutcome::Refused`].
//!
//! What "the existing fact's trust" means (see [`existing_fact_trust`]): the
//! row's stored `origin_trust` — so a value lowered after the fact (0.1 after
//! a rejected quarantine / a poisoned-source cascade) is honoured — capped at
//! its origin class ceiling. The cap matters only for rows written before WP1,
//! which carry the old column default 1.0 regardless of where they came from;
//! the cap reads them the way WP1 would have stored them (an unknown/NULL
//! origin reads as `unattributed`, 0.6). Corroboration does **not** enter: the
//! ≥2-distinct-origin reaffirmation boost raises `confidence`, never
//! `origin_trust` — provenance and belief strength are separate axes, and a
//! fact repeated by many low-trust sources is still a low-trust fact.
//!
//! [`SqliteMemoryEngine::store_temporal_outcome`]: crate::engine::SqliteMemoryEngine::store_temporal_outcome

use serde::Serialize;

/// Float slack for the strict comparison. Trusts come from a small table of
/// constants and simple `min` clamps, so equal classes compare exactly; the
/// epsilon only keeps arithmetic noise from turning "equal" into "higher".
const TRUST_EPSILON: f64 = 1e-9;

/// Why a temporal write was not allowed to become the current fact.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SupersessionRefusal {
    pub subject: String,
    pub predicate: String,
    /// The currently valid fact that would have been replaced (the highest-
    /// trust one when several rows are active for the triple).
    pub existing_id: String,
    /// The existing row's `origin` column (`None` for pre-WP1 rows).
    pub existing_origin: Option<String>,
    /// The existing fact's trust as compared (see [`existing_fact_trust`]).
    pub existing_trust: f64,
    /// The refused write's origin (after the "unattributed" default).
    pub write_origin: String,
    /// The refused write's effective trust (after ceiling / `derived_from`
    /// clamps — the value it would have been stored with).
    pub write_trust: f64,
}

impl std::fmt::Display for SupersessionRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "a {} write (trust {:.2}) may not supersede current fact {} ({}, trust {:.2}) \
             for ({}, {})",
            self.write_origin,
            self.write_trust,
            self.existing_id,
            self.existing_origin.as_deref().unwrap_or("legacy"),
            self.existing_trust,
            self.subject,
            self.predicate,
        )
    }
}

/// Result of [`store_temporal_outcome`](crate::engine::SqliteMemoryEngine::store_temporal_outcome).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum TemporalWriteOutcome {
    /// The write took effect: a new row (insert / supersession / historical
    /// segment) or, for a reaffirmation, the surviving row's id.
    Stored(String),
    /// Nothing was written; the current fact is untouched.
    Refused(SupersessionRefusal),
    /// Nothing was written: the source fence refused it (a source or parent
    /// was forgotten, a parent is missing, or too many sources — P2-B).
    Fenced(crate::lineage::FenceRefusal),
}

impl TemporalWriteOutcome {
    /// The stored id, or `None` for a refusal.
    pub fn stored_id(&self) -> Option<&str> {
        match self {
            Self::Stored(id) => Some(id),
            Self::Refused(_) | Self::Fenced(_) => None,
        }
    }
}

/// Result of [`hold_refused_claim_outcome`](crate::engine::SqliteMemoryEngine::hold_refused_claim_outcome).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldClaim {
    /// The held row's id — a new row, or the identical claim already pending.
    pub id: String,
    /// `false` when an identical claim (same agent, subject, predicate and
    /// object) was already held and pending review, so nothing was written.
    pub newly_held: bool,
}

/// Result of [`promote_quarantined`](crate::engine::SqliteMemoryEngine::promote_quarantined).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PromotionReport {
    /// Held claims re-written with the reviewer's authority.
    pub promoted: usize,
    /// Held claims NOT written because the fact they conflicted with is no
    /// longer the current one; each was closed out.
    pub stale: usize,
}

/// A quarantined row that a release turned into a held claim because the
/// current fact outranks it (see
/// [`release_quarantine`](crate::engine::SqliteMemoryEngine::release_quarantine)).
/// Its review card is built from the stored row
/// ([`held_claim_view`](crate::engine::SqliteMemoryEngine::held_claim_view)).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReleaseHeld {
    /// The row's id (unchanged; now a held claim).
    pub held_id: String,
    /// `true` when this release converted it; `false` when an earlier,
    /// failed attempt already had (the retry only re-reports it).
    pub newly_converted: bool,
    /// The guard's refusal, when converted by this call.
    pub refusal: Option<SupersessionRefusal>,
}

/// A held claim as stored, for building its review card (R-H1). Text
/// fields are untrusted data.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldClaimView {
    pub id: String,
    /// The full statement text the promotion will write.
    pub content: String,
    pub subject: String,
    pub predicate: String,
    /// The value the promotion will write (graph ranking / `get_at` read it).
    pub object: Option<String>,
    /// The protected fact recorded at hold time.
    pub conflicts_with: Option<String>,
    pub existing_content: Option<String>,
    pub existing_object: Option<String>,
    /// Converted by a burst release (rather than held at distillation).
    pub held_from_release: bool,
    /// [`claim_digest`] of this row — recorded on the card, re-checked at
    /// promotion.
    pub claim_digest: String,
}

/// SHA-256 (hex) over a held claim's content, subject, predicate and object,
/// each length-prefixed so no two different tuples share an encoding. Binds a
/// review card to the exact row it describes.
pub fn claim_digest(content: &str, subject: &str, predicate: &str, object: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for part in [Some(content), Some(subject), Some(predicate), object] {
        match part {
            Some(p) => {
                h.update([1u8]);
                h.update((p.len() as u64).to_le_bytes());
                h.update(p.as_bytes());
            }
            None => h.update([0u8]),
        }
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Result of [`release_quarantine`](crate::engine::SqliteMemoryEngine::release_quarantine).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ReleaseReport {
    /// Rows released (made current, released as history, or reaffirming the
    /// fact already current).
    pub released: usize,
    /// Rows the supersession guard refused, now held claims needing their own
    /// review.
    pub held: Vec<ReleaseHeld>,
}

/// The trust a currently valid row holds for the supersession comparison:
/// stored `origin_trust`, capped at the ceiling of its origin class (NULL /
/// unknown origin → `unattributed`).
pub fn existing_fact_trust(stored_trust: f64, origin: Option<&str>) -> f64 {
    stored_trust
        .clamp(0.0, 1.0)
        .min(crate::origin::trust_ceiling(origin.unwrap_or("")))
}

/// One currently valid, non-quarantined row competing with a write.
pub struct ActiveRow<'a> {
    pub id: &'a str,
    pub origin: Option<&'a str>,
    pub stored_trust: f64,
}

/// Decide whether a write of `write_trust` may supersede `active`. Returns
/// the refusal for the highest-trust row when any of them is strictly more
/// trusted than the write; `None` means the supersession may proceed.
pub fn check_supersession<'a>(
    subject: &str,
    predicate: &str,
    write_origin: &str,
    write_trust: f64,
    active: impl IntoIterator<Item = ActiveRow<'a>>,
) -> Option<SupersessionRefusal> {
    let mut strongest: Option<(ActiveRow<'a>, f64)> = None;
    for row in active {
        let t = existing_fact_trust(row.stored_trust, row.origin);
        if strongest.as_ref().is_none_or(|(_, best)| t > *best) {
            strongest = Some((row, t));
        }
    }
    let (row, existing_trust) = strongest?;
    if existing_trust > write_trust + TRUST_EPSILON {
        Some(SupersessionRefusal {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            existing_id: row.id.to_string(),
            existing_origin: row.origin.map(str::to_string),
            existing_trust,
            write_origin: write_origin.to_string(),
            write_trust,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::origin::{
        AGENT_DERIVED, CHANNEL_DISTILL, IMPORT, MCP_EXTERNAL, OPERATOR, TOOL_ECHO, UNATTRIBUTED,
        USER_DIRECT,
    };

    fn row(origin: &str, trust: f64) -> ActiveRow<'_> {
        ActiveRow {
            id: "existing",
            origin: Some(origin),
            stored_trust: trust,
        }
    }

    /// Every ordered pair of origin classes at their ceilings: lower → higher
    /// refused, equal and higher → lower allowed.
    #[test]
    fn origin_pair_table() {
        let classes = [
            USER_DIRECT,
            OPERATOR,
            IMPORT,
            AGENT_DERIVED,
            UNATTRIBUTED,
            TOOL_ECHO,
            CHANNEL_DISTILL,
            MCP_EXTERNAL,
        ];
        for existing in classes {
            for write in classes {
                let r = check_supersession(
                    "s",
                    "p",
                    write.name,
                    write.ceiling,
                    [row(existing.name, existing.ceiling)],
                );
                let should_refuse = existing.ceiling > write.ceiling;
                assert_eq!(
                    r.is_some(),
                    should_refuse,
                    "existing {} ({}) vs write {} ({})",
                    existing.name,
                    existing.ceiling,
                    write.name,
                    write.ceiling
                );
                if let Some(r) = r {
                    assert_eq!(r.existing_trust, existing.ceiling);
                    assert_eq!(r.write_trust, write.ceiling);
                    assert_eq!(r.existing_origin.as_deref(), Some(existing.name));
                }
            }
        }
    }

    #[test]
    fn lowered_trust_is_honoured() {
        // An operator row lowered to 0.1 (poisoned-source cascade) no longer
        // outranks a channel write.
        assert!(check_supersession("s", "p", "channel", 0.3, [row("operator", 0.1)]).is_none());
    }

    #[test]
    fn legacy_rows_are_capped_at_their_class_ceiling() {
        // Pre-WP1 row: column default 1.0, NULL origin → read as unattributed.
        let legacy = ActiveRow {
            id: "old",
            origin: None,
            stored_trust: 1.0,
        };
        assert!(check_supersession("s", "p", "agent_derived", 0.6, [legacy]).is_none());
        let legacy = ActiveRow {
            id: "old",
            origin: None,
            stored_trust: 1.0,
        };
        let r = check_supersession("s", "p", "channel", 0.3, [legacy]).unwrap();
        assert_eq!(r.existing_trust, 0.6);
        // A pre-WP1 agent row stored at 1.0 is read as 0.6.
        assert_eq!(existing_fact_trust(1.0, Some("agent_derived")), 0.6);
    }

    #[test]
    fn strongest_of_several_active_rows_decides() {
        let rows = [
            ActiveRow { id: "a", origin: Some("channel"), stored_trust: 0.3 },
            ActiveRow { id: "b", origin: Some("operator"), stored_trust: 1.0 },
        ];
        let r = check_supersession("s", "p", "import", 0.7, rows).unwrap();
        assert_eq!(r.existing_id, "b");
    }

    #[test]
    fn claim_digest_binds_every_field() {
        let base = claim_digest("c", "s", "p", Some("o"));
        assert_eq!(base.len(), 64);
        assert_eq!(base, claim_digest("c", "s", "p", Some("o")));
        assert_ne!(base, claim_digest("c2", "s", "p", Some("o")));
        assert_ne!(base, claim_digest("c", "s", "p", Some("o2")));
        assert_ne!(base, claim_digest("c", "s", "p", None));
        assert_ne!(base, claim_digest("c", "s2", "p", Some("o")));
        // Length prefixes keep shifted boundaries apart.
        assert_ne!(claim_digest("ab", "c", "p", None), claim_digest("a", "bc", "p", None));
    }

    #[test]
    fn no_active_rows_never_refuses() {
        assert!(check_supersession("s", "p", "channel", 0.3, []).is_none());
    }

    #[test]
    fn caller_lowered_write_trust_counts() {
        // A user_direct write that declared 0.5 is a 0.5 write.
        assert!(check_supersession("s", "p", "user_direct", 0.5, [row("import", 0.7)]).is_some());
    }
}
