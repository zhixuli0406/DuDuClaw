// mcp_headers.rs — ADR-002: x-duduclaw header versioning + Capability Registry (W22-P0)
//
// Provides:
//   - CAPABILITY_REGISTRY: static list of all DuDuClaw capabilities with runtime toggles
//   - API_VERSION: HTTP API compatibility version (independent from DuDuClaw SemVer)
//   - build_capabilities_header(): x-duduclaw-capabilities value (memory-first, then alpha)
//   - parse_capabilities(): parse client-sent capability header into (name, version) pairs
//   - validate_client_capabilities(): negotiation — permissive if absent, 422 if unmet
//   - build_missing_capabilities_header(): x-duduclaw-missing-capabilities value for 422

use axum::http::HeaderValue;

// ── Capability entry ──────────────────────────────────────────────────────────

/// A single capability entry in the DuDuClaw capability registry.
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityEntry {
    /// Capability name (e.g., "memory", "mcp", "a2a")
    pub name: &'static str,
    /// Major version of this capability.
    /// Starts at 1; increments only on breaking API/protocol changes (ADR-002 §4.2).
    pub major_version: u32,
    /// Runtime toggle — `false` = feature-flagged off (not yet shipped); `true` = live.
    /// Disabled capabilities are never included in outbound headers (ADR-002 §7).
    pub enabled: bool,
}

// ── Capability registry ───────────────────────────────────────────────────────

/// Static capability registry — updated each Sprint when capabilities ship or major-bump.
///
/// Internal ordering is alphabetical for maintainability.
/// Header output follows ADR-002 §4.1 sort rule:
///   `memory` always first, remaining capabilities in lexicographic order.
pub const CAPABILITY_REGISTRY: &[CapabilityEntry] = &[
    CapabilityEntry { name: "a2a",            major_version: 1, enabled: false }, // W21 — A2A Bridge (pending enablement)
    CapabilityEntry { name: "audit",          major_version: 2, enabled: true  }, // W20-P1
    CapabilityEntry { name: "governance",     major_version: 1, enabled: true  }, // W19-P1
    CapabilityEntry { name: "memory",         major_version: 3, enabled: true  }, // core — always first in header
    CapabilityEntry { name: "mcp",            major_version: 2, enabled: true  }, // W20 HTTP/SSE Phase 2
    CapabilityEntry { name: "secret-manager", major_version: 1, enabled: false }, // W22 P0 — pending
    CapabilityEntry { name: "signed-card",    major_version: 1, enabled: false }, // W22 P1 — pending
    CapabilityEntry { name: "skill",          major_version: 1, enabled: true  },
    CapabilityEntry { name: "wiki",           major_version: 1, enabled: true  },
];

/// The HTTP API compatibility version — independent from DuDuClaw SemVer (ADR-002 §4.3).
///
/// - MAJOR `1`: HTTP API is stable (past beta)
/// - MINOR `2`: Second backward-compatible HTTP upgrade (W22 capability negotiation)
/// Only changes on actual HTTP API compatibility changes, not on DuDuClaw MINOR/PATCH bumps.
pub const API_VERSION: &str = "1.2";

// ── Header builder ─────────────────────────────────────────────────────────────

/// Build the `x-duduclaw-capabilities` header value from an arbitrary registry slice.
///
/// Exposed for testing with custom registries (e.g., all-disabled, single-entry scenarios).
///
/// Rules (ADR-002 §4.1):
/// 1. Only `enabled: true` entries are included
/// 2. `memory` is always emitted first (DuDuClaw core differentiator)
/// 3. All other enabled capabilities follow in lexicographic (ASCII) order
pub fn build_capabilities_header_from(registry: &[CapabilityEntry]) -> String {
    let memory = registry.iter().find(|c| c.name == "memory" && c.enabled);

    let mut others: Vec<&CapabilityEntry> = registry
        .iter()
        .filter(|c| c.name != "memory" && c.enabled)
        .collect();
    others.sort_by_key(|c| c.name);

    let mut parts: Vec<String> = Vec::new();
    if let Some(m) = memory {
        parts.push(format!("{}/{}", m.name, m.major_version));
    }
    for cap in others {
        parts.push(format!("{}/{}", cap.name, cap.major_version));
    }
    parts.join(",")
}

/// Build the `x-duduclaw-capabilities` header value from the default [`CAPABILITY_REGISTRY`].
///
/// Format: `memory/<v>,<other-caps-alpha-sorted>/<v>,...`
/// Only enabled capabilities appear. Disabled ones are silently omitted (ADR-002 §7).
pub fn build_capabilities_header() -> String {
    build_capabilities_header_from(CAPABILITY_REGISTRY)
}

// ── Capability parsing ─────────────────────────────────────────────────────────

/// A parsed capability requirement: `(capability_name, required_major_version)`.
pub type ParsedCapability = (String, u32);

/// Parse an `x-duduclaw-capabilities` header value into `(name, version)` pairs.
///
/// Accepts: `"a2a/1,mcp/2"` → `[("a2a", 1), ("mcp", 2)]`
///
/// Returns `Err(String)` if the header contains invalid UTF-8 or a malformed entry.
/// An empty header value returns `Ok(vec![])` (no requirements).
pub fn parse_capabilities(header_val: &HeaderValue) -> Result<Vec<ParsedCapability>, String> {
    let raw = header_val
        .to_str()
        .map_err(|e| format!("Invalid header encoding: {e}"))?;

    if raw.is_empty() {
        return Ok(vec![]);
    }

    raw.split(',')
        .map(|item| {
            let item = item.trim();
            let mut parts = item.splitn(2, '/');
            let name = parts.next().unwrap_or("").trim();
            let version_str = parts.next().unwrap_or("").trim();

            if name.is_empty() || version_str.is_empty() {
                return Err(format!("Malformed capability entry: '{item}'"));
            }

            let version = version_str
                .parse::<u32>()
                .map_err(|_| format!("Invalid capability version in '{item}': expected u32"))?;

            Ok((name.to_string(), version))
        })
        .collect()
}

// ── Capability negotiation ─────────────────────────────────────────────────────

/// A capability that the server cannot satisfy for the client's stated requirement.
#[derive(Debug, Clone, PartialEq)]
pub struct MissingCapability {
    pub capability: String,
    pub required_version: u32,
    /// `None` if the capability is completely absent or disabled on this server instance.
    /// `Some(v)` if the capability exists but its major version is below what was requested.
    pub server_version: Option<u32>,
}

/// Error returned when capability negotiation fails (→ HTTP 422 Unprocessable Entity).
#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityMismatchError {
    pub missing: Vec<MissingCapability>,
}

impl std::fmt::Display for CapabilityMismatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Required capabilities not available: ")?;
        for (i, m) in self.missing.iter().enumerate() {
            if i > 0 {
                write!(f, ", ")?;
            }
            match m.server_version {
                None => write!(f, "{} (not available)", m.capability)?,
                Some(sv) => write!(
                    f,
                    "{} (required v{}, server v{})",
                    m.capability, m.required_version, sv
                )?,
            }
        }
        Ok(())
    }
}

/// Validate client's capability requirements against the server's [`CAPABILITY_REGISTRY`].
///
/// **Permissive mode**: if `request_header` is `None`, all requests pass through.
/// **Malformed header**: treated permissively (don't block clients on parse failures).
///
/// Returns `Err(CapabilityMismatchError)` listing each unsatisfied capability when:
/// - A required capability is disabled or absent on this server, OR
/// - The server's major version for that capability is lower than what was requested
pub fn validate_client_capabilities(
    request_header: Option<&HeaderValue>,
) -> Result<(), CapabilityMismatchError> {
    let Some(header_val) = request_header else {
        return Ok(()); // permissive: client declared no requirements
    };

    let requested = match parse_capabilities(header_val) {
        Ok(caps) => caps,
        Err(_) => return Ok(()), // malformed header → permissive (don't block)
    };

    if requested.is_empty() {
        return Ok(());
    }

    let mut missing = Vec::new();

    for (name, required_version) in &requested {
        match CAPABILITY_REGISTRY.iter().find(|c| c.name == name.as_str()) {
            // Capability unknown to this server or explicitly disabled
            None | Some(CapabilityEntry { enabled: false, .. }) => {
                missing.push(MissingCapability {
                    capability: name.clone(),
                    required_version: *required_version,
                    server_version: None,
                });
            }
            // Capability present but version too old
            Some(cap) if cap.major_version < *required_version => {
                missing.push(MissingCapability {
                    capability: name.clone(),
                    required_version: *required_version,
                    server_version: Some(cap.major_version),
                });
            }
            // Capability present and version sufficient — OK
            _ => {}
        }
    }

    if missing.is_empty() {
        Ok(())
    } else {
        Err(CapabilityMismatchError { missing })
    }
}

/// Build the `x-duduclaw-missing-capabilities` header value for 422 error responses.
///
/// Format: `<name>/<required_version>,...` — only the unsatisfied entries.
/// Per ADR-002 §7: only lists what the *client explicitly requested*, not the full server list.
pub fn build_missing_capabilities_header(missing: &[MissingCapability]) -> String {
    missing
        .iter()
        .map(|m| format!("{}/{}", m.capability, m.required_version))
        .collect::<Vec<_>>()
        .join(",")
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    // ── build_capabilities_header tests (≥ 5 required by ADR-002) ─────────────

    #[test]
    fn header_memory_is_first() {
        let header = build_capabilities_header();
        assert!(
            header.starts_with("memory/"),
            "memory must be first in header, got: {header}"
        );
    }

    #[test]
    fn header_only_enabled_capabilities_included() {
        let header = build_capabilities_header();
        // Disabled capabilities must NOT appear in the outbound header
        assert!(!header.contains("a2a/"), "disabled 'a2a' must not appear: {header}");
        assert!(!header.contains("secret-manager/"), "disabled 'secret-manager' must not appear: {header}");
        assert!(!header.contains("signed-card/"), "disabled 'signed-card' must not appear: {header}");
    }

    #[test]
    fn header_non_memory_caps_are_alpha_sorted() {
        let header = build_capabilities_header();
        let parts: Vec<&str> = header.split(',').collect();
        assert!(parts.len() > 1, "should have more than memory alone");
        // First must be memory
        assert!(parts[0].starts_with("memory/"), "first entry must be memory/");
        // Rest must be in lexicographic order by name
        let rest_names: Vec<&str> = parts[1..].iter()
            .map(|s| s.split('/').next().unwrap_or(""))
            .collect();
        let mut sorted = rest_names.clone();
        sorted.sort();
        assert_eq!(rest_names, sorted, "non-memory capabilities must be lexicographically ordered");
    }

    #[test]
    fn header_entries_follow_name_slash_version_format() {
        let header = build_capabilities_header();
        for part in header.split(',') {
            let mut iter = part.splitn(2, '/');
            let name = iter.next().expect("name segment missing");
            let ver = iter.next().expect("version segment missing");
            assert!(!name.is_empty(), "empty name in entry '{part}'");
            assert!(ver.parse::<u32>().is_ok(), "non-numeric version in entry '{part}'");
        }
    }

    /// Snapshot test — prevents silent capability registry changes from breaking downstream
    /// integrations. Update this string ONLY when intentionally changing the registry.
    #[test]
    fn header_snapshot_matches_expected() {
        let expected = "memory/3,audit/2,governance/1,mcp/2,skill/1,wiki/1";
        let actual = build_capabilities_header();
        assert_eq!(
            actual, expected,
            "capabilities header snapshot changed — update this test only when the registry \
             intentionally changes (ADR-002 §8: SDK doc sync required)"
        );
    }

    #[test]
    fn header_empty_when_all_disabled() {
        // Use build_capabilities_header_from with an all-disabled registry
        let all_disabled = &[
            CapabilityEntry { name: "memory", major_version: 3, enabled: false },
            CapabilityEntry { name: "mcp",    major_version: 2, enabled: false },
        ];
        let header = build_capabilities_header_from(all_disabled);
        assert_eq!(header, "", "all-disabled registry must produce empty header");
    }

    #[test]
    fn header_memory_disabled_omits_it_and_keeps_alpha_order() {
        // memory disabled → no memory first; remaining caps are still alpha sorted
        let registry = &[
            CapabilityEntry { name: "memory", major_version: 3, enabled: false },
            CapabilityEntry { name: "wiki",   major_version: 1, enabled: true },
            CapabilityEntry { name: "audit",  major_version: 2, enabled: true },
        ];
        let header = build_capabilities_header_from(registry);
        assert_eq!(header, "audit/2,wiki/1", "without memory, remaining caps must be alpha sorted");
    }

    // ── parse_capabilities tests ──────────────────────────────────────────────

    #[test]
    fn parse_valid_single_entry() {
        let hv = HeaderValue::from_static("mcp/2");
        let result = parse_capabilities(&hv).unwrap();
        assert_eq!(result, vec![("mcp".to_string(), 2)]);
    }

    #[test]
    fn parse_valid_multiple_entries() {
        let hv = HeaderValue::from_static("a2a/1,mcp/2,memory/3");
        let result = parse_capabilities(&hv).unwrap();
        assert_eq!(result.len(), 3);
        assert!(result.contains(&("a2a".to_string(), 1)));
        assert!(result.contains(&("mcp".to_string(), 2)));
        assert!(result.contains(&("memory".to_string(), 3)));
    }

    #[test]
    fn parse_empty_header_returns_empty_vec() {
        let hv = HeaderValue::from_static("");
        let result = parse_capabilities(&hv).unwrap();
        assert!(result.is_empty(), "empty header should parse to empty vec");
    }

    #[test]
    fn parse_malformed_version_returns_error() {
        let hv = HeaderValue::from_static("mcp/notanumber");
        let result = parse_capabilities(&hv);
        assert!(result.is_err(), "non-numeric version must return Err");
    }

    // ── validate_client_capabilities tests (≥ 8 required by ADR-002) ──────────

    #[test]
    fn validate_permissive_when_no_header() {
        // No x-duduclaw-capabilities in request → always pass through
        let result = validate_client_capabilities(None);
        assert!(result.is_ok(), "permissive mode: absent header must always succeed");
    }

    #[test]
    fn validate_empty_header_value_is_permissive() {
        let hv = HeaderValue::from_static("");
        let result = validate_client_capabilities(Some(&hv));
        assert!(result.is_ok(), "empty header value must be treated as no requirement");
    }

    #[test]
    fn validate_all_requirements_fully_satisfied() {
        // mcp/2 and memory/3 are both enabled at exactly the required versions
        let hv = HeaderValue::from_static("mcp/2,memory/3");
        let result = validate_client_capabilities(Some(&hv));
        assert!(result.is_ok(), "all requested capabilities should be satisfied");
    }

    #[test]
    fn validate_capability_not_in_registry_is_missing() {
        let hv = HeaderValue::from_static("unknown-feature/1");
        let err = validate_client_capabilities(Some(&hv)).unwrap_err();
        assert_eq!(err.missing.len(), 1);
        assert_eq!(err.missing[0].capability, "unknown-feature");
        assert_eq!(err.missing[0].server_version, None, "unknown cap has no server version");
    }

    #[test]
    fn validate_disabled_capability_treated_as_missing() {
        // a2a is in the registry but enabled: false → treated as missing
        let hv = HeaderValue::from_static("a2a/1");
        let err = validate_client_capabilities(Some(&hv)).unwrap_err();
        assert_eq!(err.missing.len(), 1);
        assert_eq!(err.missing[0].capability, "a2a");
        assert_eq!(err.missing[0].server_version, None, "disabled cap → server_version None");
    }

    #[test]
    fn validate_version_requirement_too_high_returns_mismatch() {
        // mcp is at major_version 2; client requests 3 → version conflict
        let hv = HeaderValue::from_static("mcp/3");
        let err = validate_client_capabilities(Some(&hv)).unwrap_err();
        assert_eq!(err.missing.len(), 1);
        assert_eq!(err.missing[0].capability, "mcp");
        assert_eq!(err.missing[0].required_version, 3);
        assert_eq!(err.missing[0].server_version, Some(2), "server version should be reported");
    }

    #[test]
    fn validate_partial_missing_reports_only_unsatisfied() {
        // memory/3 is satisfied, secret-manager/1 is disabled
        let hv = HeaderValue::from_static("memory/3,secret-manager/1");
        let err = validate_client_capabilities(Some(&hv)).unwrap_err();
        assert_eq!(err.missing.len(), 1, "only one capability should be missing");
        assert_eq!(err.missing[0].capability, "secret-manager");
    }

    #[test]
    fn validate_multiple_missing_all_reported() {
        // a2a and secret-manager are both disabled
        let hv = HeaderValue::from_static("a2a/1,secret-manager/1");
        let err = validate_client_capabilities(Some(&hv)).unwrap_err();
        assert_eq!(err.missing.len(), 2, "both missing capabilities must be reported");
    }

    #[test]
    fn validate_malformed_header_is_permissive() {
        // No '/' separator → parse_capabilities returns Err → permissive fallback
        let hv = HeaderValue::from_static("badformat");
        let result = validate_client_capabilities(Some(&hv));
        assert!(result.is_ok(), "malformed header must not block the request (permissive fallback)");
    }

    // ── build_missing_capabilities_header tests ───────────────────────────────

    #[test]
    fn missing_header_format_is_correct() {
        let missing = vec![
            MissingCapability {
                capability: "a2a".to_string(),
                required_version: 1,
                server_version: None,
            },
            MissingCapability {
                capability: "secret-manager".to_string(),
                required_version: 1,
                server_version: None,
            },
        ];
        let header = build_missing_capabilities_header(&missing);
        assert_eq!(header, "a2a/1,secret-manager/1");
    }

    #[test]
    fn missing_header_empty_for_no_missing() {
        let header = build_missing_capabilities_header(&[]);
        assert_eq!(header, "", "no missing capabilities → empty header value");
    }

    // ── CapabilityMismatchError Display tests ──────────────────────────────────

    #[test]
    fn mismatch_display_shows_capability_names() {
        let err = CapabilityMismatchError {
            missing: vec![
                MissingCapability {
                    capability: "a2a".to_string(),
                    required_version: 1,
                    server_version: None,
                },
            ],
        };
        let display = err.to_string();
        assert!(display.contains("a2a"), "display must mention missing capability name");
        assert!(display.contains("not available"), "must describe absent capability");
    }
}
