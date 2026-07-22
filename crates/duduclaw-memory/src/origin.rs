//! Origin classification for memory writes (WP1 TMA-NM, arXiv:2606.24322).
//!
//! Every memory write carries an `origin` string naming *where the fact came
//! from*. This module maps that string to a **trust ceiling** — the maximum
//! `origin_trust` a fact from that source may ever hold. `store_temporal`
//! clamps the caller-declared trust down to this ceiling (I8 non-malleability:
//! a caller can only *lower* trust, never claim more than its provenance
//! warrants — re-derivation/summarization cannot launder a low-trust fact into
//! a higher-trust one).
//!
//! Fail-safe: an unrecognized origin maps to [`UNATTRIBUTED`] (ceiling `0.6`),
//! **never** to full trust. A caller that forgets to set an origin therefore
//! degrades gracefully instead of silently minting a maximally-trusted fact.

/// A canonical origin class: a stable name plus its `origin_trust` ceiling.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OriginClass {
    /// Stable class name (also the string written to the `origin` DB column so
    /// D6 curation can roll back by origin).
    pub name: &'static str,
    /// Maximum `origin_trust` a fact from this source may hold (`[0,1]`).
    pub ceiling: f64,
}

// ── The classification table (WP1 design §1) ────────────────────────────────
// (name, trust ceiling) — a caller can declare LESS trust but never more.

/// The user typed it themselves — the highest-trust provenance.
pub const USER_DIRECT: OriginClass = OriginClass {
    name: "user_direct",
    ceiling: 1.0,
};
/// A dashboard / CLI operator action.
pub const OPERATOR: OriginClass = OriginClass {
    name: "operator",
    ceiling: 1.0,
};
/// Bulk migrate / import of pre-existing records.
pub const IMPORT: OriginClass = OriginClass {
    name: "import",
    ceiling: 0.7,
};
/// Agent self-derived content (reflexion, skill synthesis, decision capture,
/// episodic prediction store, mood logs, night consolidation).
pub const AGENT_DERIVED: OriginClass = OriginClass {
    name: "agent_derived",
    ceiling: 0.6,
};
/// A summary of a tool's output.
pub const TOOL_ECHO: OriginClass = OriginClass {
    name: "tool_echo",
    ceiling: 0.5,
};
/// Conversation distillation — the lowest-trust tier (unverified chat input).
pub const CHANNEL_DISTILL: OriginClass = OriginClass {
    name: "channel",
    ceiling: 0.3,
};
/// An `external/` namespace MCP write from an untrusted client.
pub const MCP_EXTERNAL: OriginClass = OriginClass {
    name: "mcp_external",
    ceiling: 0.3,
};
/// Unlabelled writes (legacy paths mid-migration). Fail-safe ceiling `0.6` —
/// the default for any origin we do not recognize.
pub const UNATTRIBUTED: OriginClass = OriginClass {
    name: "unattributed",
    ceiling: 0.6,
};

/// Map a raw origin string to its canonical [`OriginClass`].
///
/// Unknown origins (including empty strings) resolve to [`UNATTRIBUTED`] —
/// fail-safe, never full trust. `"user"` / `"user_profile"` are legacy aliases
/// for user-direct provenance (predate the canonical table) and keep their
/// historical `1.0` ceiling so existing curated/profile facts do not regress.
pub fn classify(origin: &str) -> OriginClass {
    match origin {
        "user_direct" | "user" | "user_profile" => USER_DIRECT,
        "operator" => OPERATOR,
        "import" => IMPORT,
        "agent_derived" => AGENT_DERIVED,
        "tool_echo" => TOOL_ECHO,
        "channel" => CHANNEL_DISTILL,
        "mcp_external" => MCP_EXTERNAL,
        "unattributed" => UNATTRIBUTED,
        // Fail-safe: an unrecognized origin can never claim full trust.
        _ => UNATTRIBUTED,
    }
}

/// The maximum `origin_trust` a fact from `origin` may hold. Unknown → `0.6`.
pub fn trust_ceiling(origin: &str) -> f64 {
    classify(origin).ceiling
}

/// Canonical class name for distinctness comparisons (Sybil-resistant
/// corroboration): every unknown origin collapses to `"unattributed"`, so N
/// forged channel names count as **one** class, not N.
pub fn class_name(origin: &str) -> &'static str {
    classify(origin).name
}

/// Whether an origin may contribute to confidence-raising corroboration.
///
/// Agent-derived and tool-echo facts are self-referential / low-independence,
/// so they never count toward the ≥2-distinct-origin boost gate (WP1 §3): an
/// agent re-observing its own conclusion is not independent evidence.
pub fn is_corroborating(origin: &str) -> bool {
    let c = classify(origin);
    c != AGENT_DERIVED && c != TOOL_ECHO
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_origin_fails_safe_to_unattributed() {
        // The load-bearing guarantee: an unknown origin is never fully trusted.
        assert_eq!(classify("who-knows"), UNATTRIBUTED);
        assert_eq!(trust_ceiling("who-knows"), 0.6);
        assert!((trust_ceiling("") - 0.6).abs() < f64::EPSILON);
        assert!(trust_ceiling("anything") < 1.0);
    }

    #[test]
    fn known_ceilings_match_table() {
        assert_eq!(trust_ceiling("user_direct"), 1.0);
        assert_eq!(trust_ceiling("operator"), 1.0);
        assert_eq!(trust_ceiling("import"), 0.7);
        assert_eq!(trust_ceiling("agent_derived"), 0.6);
        assert_eq!(trust_ceiling("tool_echo"), 0.5);
        assert_eq!(trust_ceiling("channel"), 0.3);
        assert_eq!(trust_ceiling("mcp_external"), 0.3);
        assert_eq!(trust_ceiling("unattributed"), 0.6);
    }

    #[test]
    fn legacy_aliases_keep_user_direct_trust() {
        assert_eq!(classify("user"), USER_DIRECT);
        assert_eq!(classify("user_profile"), USER_DIRECT);
    }

    #[test]
    fn distinct_class_names_collapse_unknowns() {
        // Forged channel names are one class, not many (Sybil resistance).
        assert_eq!(class_name("chan-a"), class_name("chan-b"));
        assert_ne!(class_name("channel"), class_name("import"));
    }

    #[test]
    fn corroboration_excludes_self_derived() {
        assert!(!is_corroborating("agent_derived"));
        assert!(!is_corroborating("tool_echo"));
        assert!(is_corroborating("channel"));
        assert!(is_corroborating("operator"));
        assert!(is_corroborating("import"));
    }
}
