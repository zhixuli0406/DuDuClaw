//! Action Claim Verifier — cross-references agent output claims against
//! actual MCP tool call records to detect tool-use hallucination.
//!
//! Inspired by:
//! - Grid-Mind (arXiv 2602.20683): Forced Tool Routing + Post-response Grounding
//! - AgentSpec (ICSE 2026, arXiv 2503.18666): Runtime Constraint Enforcement
//! - AgentHallu (arXiv 2601.06818): Tool-use hallucination classification
//!
//! Design: Zero LLM cost — pure regex matching + log cross-reference.

use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

/// Pre-compiled regex pattern paired with its claim type.
struct ClaimPattern {
    re: Regex,
    claim_type: ClaimType,
}

/// Agent ID character class for regex: lowercase alphanumeric + hyphens.
/// Matches the validation in `is_valid_agent_id()` (mcp.rs).
const AGENT_ID_RE: &str = "[a-z][a-z0-9-]{0,63}";

/// Maximum hallucination claims per response to prevent DoS via adversarial output.
const MAX_CLAIMS_PER_RESPONSE: usize = 10;

/// All action claim patterns, compiled once at first use.
///
/// Chinese patterns use two variants:
/// - Bracketed: `「agent-name」` with CJK quotation marks as anchors
/// - Unbracketed: `agent agent-name` where the ID is constrained by `AGENT_ID_RE`
///   (greedy, stops at first char that's not `[a-z0-9-]`)
///
/// This avoids the lazy `\S+?` pitfall where non-greedy quantifiers with
/// optional trailing anchors capture only 1 character.
static CLAIM_PATTERNS: LazyLock<Vec<ClaimPattern>> = LazyLock::new(|| {
    let id = AGENT_ID_RE;
    let specs: Vec<(String, ClaimType)> = vec![
        // ── Agent creation ───────────────────────────────────
        // Chinese — bracketed: 「tl-xianwen」
        (format!(r"(?:已建立|建立完成|成功建立|已創建|創建完成|成功創建)\s*(?:了\s*)?(?:agent|Agent|代理)\s*「({id})」"), ClaimType::AgentCreated),
        // Chinese — unbracketed: agent tl-xianwen (greedy ID until non-ID char)
        (format!(r"(?:已建立|建立完成|成功建立|已創建|創建完成|成功創建)\s*(?:了\s*)?(?:agent|Agent|代理)\s+({id})"), ClaimType::AgentCreated),
        // Chinese — reverse order: 「tl-xianwen」已建立
        (format!(r"「({id})」\s*(?:已建立|建立完成|建立成功|已創建|創建成功)"), ClaimType::AgentCreated),
        // English
        (format!(r#"(?i)(?:created|set up|established)\s+(?:agent\s+)?["']?({id})["']?\s*(?:successfully)?"#), ClaimType::AgentCreated),
        (format!(r#"(?i)Agent\s+["']?({id})["']?\s*(?:\([^)]+\)\s*)?created\s+successfully"#), ClaimType::AgentCreated),

        // ── Agent deletion ───────────────────────────────────
        (format!(r"(?:已刪除|刪除完成|成功刪除|已移除|移除完成)\s*(?:了\s*)?(?:agent|Agent|代理)\s*「({id})」"), ClaimType::AgentDeleted),
        (format!(r"(?:已刪除|刪除完成|成功刪除|已移除|移除完成)\s*(?:了\s*)?(?:agent|Agent|代理)\s+({id})"), ClaimType::AgentDeleted),
        (format!(r#"(?i)(?:deleted|removed)\s+(?:agent\s+)?["']?({id})["']?"#), ClaimType::AgentDeleted),

        // ── SOUL update ──────────────────────────────────────
        // With optional agent_id: 已更新 agent-name 的 SOUL.md
        (format!(r"(?:已更新|更新完成|成功更新|已修改)\s*(?:了\s*)?({id})\s*(?:的\s*)?(?:SOUL\.md|靈魂|人格)"), ClaimType::SoulUpdated),
        // Without agent_id: 已更新 SOUL.md
        (r"(?:已更新|更新完成|成功更新|已修改)\s*(?:了\s*)?(?:SOUL\.md|靈魂|人格)".to_string(), ClaimType::SoulUpdated),
        (r"(?i)(?:updated|modified)\s+SOUL\.md".to_string(), ClaimType::SoulUpdated),

        // ── Message sent ─────────────────────────────────────
        (format!(r"(?:已發送|已傳送)\s*(?:訊息|消息)\s*(?:給|到)\s*「({id})」"), ClaimType::MessageSent),
        (format!(r"(?:已發送|已傳送)\s*(?:訊息|消息)\s*(?:給|到)\s*({id})"), ClaimType::MessageSent),
        (format!(r#"(?i)(?:sent|forwarded)\s+(?:message\s+)?to\s+(?:agent\s+)?["']?({id})["']?"#), ClaimType::MessageSent),

        // ── Spawn ────────────────────────────────────────────
        (format!(r"(?:已派遣|已指派|已委派)\s*(?:任務\s*)?(?:給\s*)?「({id})」"), ClaimType::AgentSpawned),
        (format!(r"(?:已派遣|已指派|已委派)\s*(?:任務\s*)?(?:給\s*)?({id})"), ClaimType::AgentSpawned),
        (format!(r#"(?i)(?:spawned|delegated)\s+(?:task\s+)?(?:to\s+)?(?:agent\s+)?["']?({id})["']?"#), ClaimType::AgentSpawned),
    ];

    specs
        .into_iter()
        .filter_map(|(pat, ct)| {
            Regex::new(&pat).ok().map(|re| ClaimPattern {
                re,
                claim_type: ct,
            })
        })
        .collect()
});

/// The type of action an agent claims to have performed.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimType {
    /// Agent claims to have created a sub-agent.
    AgentCreated,
    /// Agent claims to have deleted/removed an agent.
    AgentDeleted,
    /// Agent claims to have updated an agent's SOUL.md.
    SoulUpdated,
    /// Agent claims to have sent a message to another agent.
    MessageSent,
    /// Agent claims to have spawned a sub-agent task.
    AgentSpawned,
}

impl ClaimType {
    /// The MCP tool name that must appear in the audit log for this claim.
    pub fn expected_tool(&self) -> &str {
        match self {
            Self::AgentCreated => "create_agent",
            Self::AgentDeleted => "agent_remove",
            Self::SoulUpdated => "agent_update_soul",
            Self::MessageSent => "send_to_agent",
            Self::AgentSpawned => "spawn_agent",
        }
    }
}

/// A single action claim extracted from agent output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionClaim {
    pub claim_type: ClaimType,
    /// The target identifier mentioned in the claim (e.g., agent name).
    pub target_id: String,
    /// The raw text fragment that matched.
    pub matched_text: String,
}

/// Result of verifying a single claim against the tool call log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status")]
pub enum VerifyResult {
    /// The claim is backed by an actual tool call.
    Verified {
        claim: ActionClaim,
    },
    /// The claim has no corresponding tool call — hallucination detected.
    Hallucination {
        claim: ActionClaim,
        reason: String,
    },
}

/// Extract action claims from agent output text.
///
/// Uses regex patterns for both Chinese (zh-TW) and English outputs.
/// Returns all claims found; duplicates are not deduplicated.
pub fn extract_action_claims(output: &str) -> Vec<ActionClaim> {
    let mut claims = Vec::new();
    // Dedup key: (claim_type, target_id) — bracketed and unbracketed patterns
    // for the same agent_id should produce only one claim (review R3-L3).
    let mut seen = std::collections::HashSet::new();

    for pattern in CLAIM_PATTERNS.iter() {
        for cap in pattern.re.captures_iter(output) {
            let target_id = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let matched_text = cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default();

            let key = (pattern.claim_type.clone(), target_id.clone());
            if !seen.insert(key) {
                continue; // duplicate claim_type + target_id, skip
            }

            claims.push(ActionClaim {
                claim_type: pattern.claim_type.clone(),
                target_id,
                matched_text,
            });
            // Cap to prevent DoS via adversarial output with hundreds of claims
            if claims.len() >= MAX_CLAIMS_PER_RESPONSE {
                return claims;
            }
        }
    }

    claims
}

/// Verify extracted claims against MCP tool call records.
///
/// `tool_calls` should be the records from `audit::read_tool_calls_since()`
/// filtered for the relevant agent and time window.
pub fn verify_claims(
    claims: &[ActionClaim],
    tool_calls: &[serde_json::Value],
) -> Vec<VerifyResult> {
    claims
        .iter()
        .map(|claim| {
            let expected_tool = claim.claim_type.expected_tool();

            let has_matching_call = tool_calls.iter().any(|record| {
                let tool_match = record
                    .get("tool_name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|name| name == expected_tool);
                let success = record
                    .get("success")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                // For claims with a target_id, also check if the tool call
                // params mention the same target.
                let target_match = if claim.target_id.is_empty() {
                    true
                } else {
                    record
                        .get("params_summary")
                        .and_then(|v| v.as_str())
                        .is_some_and(|p| p.contains(&claim.target_id))
                };

                tool_match && success && target_match
            });

            if has_matching_call {
                VerifyResult::Verified {
                    claim: claim.clone(),
                }
            } else {
                VerifyResult::Hallucination {
                    claim: claim.clone(),
                    reason: format!(
                        "Agent claimed '{}' but no successful '{}' tool call found{}",
                        claim.matched_text,
                        expected_tool,
                        if claim.target_id.is_empty() {
                            String::new()
                        } else {
                            format!(" for target '{}'", claim.target_id)
                        },
                    ),
                }
            }
        })
        .collect()
}

/// Convenience: run full verification pipeline on agent output.
///
/// Returns only hallucinated claims (empty vec = all verified or no claims).
pub fn detect_hallucinations(
    home_dir: &Path,
    agent_id: &str,
    output: &str,
    dispatch_start_time: &str,
) -> Vec<VerifyResult> {
    let claims = extract_action_claims(output);
    if claims.is_empty() {
        return Vec::new();
    }

    let tool_calls = crate::audit::read_tool_calls_since(home_dir, agent_id, dispatch_start_time);
    let results = verify_claims(&claims, &tool_calls);

    results
        .into_iter()
        .filter(|r| matches!(r, VerifyResult::Hallucination { .. }))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Chinese pattern: agent_id extraction ─────────────────

    #[test]
    fn test_chinese_agent_created_with_comma() {
        // Real-world Agnes output: agent_id followed by full-width comma
        let output = "已成功建立 agent tl-xianwen，角色為 Team Leader";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty(), "should match '成功建立 agent ...'");
        let claim = claims.iter().find(|c| c.claim_type == ClaimType::AgentCreated).unwrap();
        assert_eq!(claim.target_id, "tl-xianwen", "should capture full agent_id, not just 't'");
    }

    #[test]
    fn test_chinese_agent_created_with_brackets() {
        let output = "已建立 agent「pm-duduclaw」";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        let claim = claims.iter().find(|c| c.claim_type == ClaimType::AgentCreated).unwrap();
        assert_eq!(claim.target_id, "pm-duduclaw");
    }

    #[test]
    fn test_chinese_agent_created_end_of_line() {
        let output = "成功建立 Agent rust-engineer-xianwen";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        let claim = claims.iter().find(|c| c.claim_type == ClaimType::AgentCreated).unwrap();
        assert_eq!(claim.target_id, "rust-engineer-xianwen");
    }

    // ── English patterns ─────────────────────────────────────

    #[test]
    fn test_english_agent_created() {
        let output = "Agent 'tl-duduclaw' (Team Leader) created successfully.";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        assert!(claims.iter().any(|c| c.claim_type == ClaimType::AgentCreated
            && c.target_id == "tl-duduclaw"));
    }

    #[test]
    fn test_english_created_unquoted() {
        let output = "created agent qa-backend-xianwen successfully";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        let claim = claims.iter().find(|c| c.claim_type == ClaimType::AgentCreated).unwrap();
        assert_eq!(claim.target_id, "qa-backend-xianwen");
    }

    // ── Normal text: no false positives ──────────────────────

    #[test]
    fn test_no_claims_in_normal_text() {
        let output = "Here is the analysis of the codebase structure.";
        let claims = extract_action_claims(output);
        assert!(claims.is_empty());
    }

    #[test]
    fn test_no_claims_in_chinese_normal_text() {
        let output = "以下是程式碼架構的分析結果。";
        let claims = extract_action_claims(output);
        assert!(claims.is_empty());
    }

    // ── SoulUpdated with optional target_id ──────────────────

    #[test]
    fn test_soul_updated_with_agent_id() {
        let output = "已更新 tl-xianwen 的 SOUL.md";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        let claim = claims.iter().find(|c| c.claim_type == ClaimType::SoulUpdated).unwrap();
        assert_eq!(claim.target_id, "tl-xianwen");
    }

    #[test]
    fn test_soul_updated_without_agent_id() {
        let output = "已更新 SOUL.md";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        let claim = claims.iter().find(|c| c.claim_type == ClaimType::SoulUpdated).unwrap();
        assert!(claim.target_id.is_empty(), "no agent_id in this pattern");
    }

    // ── Verification logic ───────────────────────────────────

    #[test]
    fn test_verify_hallucination_empty_log() {
        let claims = vec![ActionClaim {
            claim_type: ClaimType::AgentCreated,
            target_id: "tl-xianwen".to_string(),
            matched_text: "created agent tl-xianwen".to_string(),
        }];
        let results = verify_claims(&claims, &[]);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], VerifyResult::Hallucination { .. }));
    }

    #[test]
    fn test_verify_success_matching_tool_call() {
        let claims = vec![ActionClaim {
            claim_type: ClaimType::AgentCreated,
            target_id: "tl-xianwen".to_string(),
            matched_text: "created agent tl-xianwen".to_string(),
        }];
        let tool_calls = vec![serde_json::json!({
            "timestamp": "2026-04-09T12:00:00Z",
            "agent_id": "agnes",
            "tool_name": "create_agent",
            "params_summary": "name=tl-xianwen display_name=TL Xianwen",
            "success": true,
        })];
        let results = verify_claims(&claims, &tool_calls);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], VerifyResult::Verified { .. }));
    }

    #[test]
    fn test_verify_wrong_agent_is_hallucination() {
        // Claim: created tl-xianwen, but tool call was for other-agent
        let claims = vec![ActionClaim {
            claim_type: ClaimType::AgentCreated,
            target_id: "tl-xianwen".to_string(),
            matched_text: "created agent tl-xianwen".to_string(),
        }];
        let tool_calls = vec![serde_json::json!({
            "timestamp": "2026-04-09T12:00:00Z",
            "agent_id": "agnes",
            "tool_name": "create_agent",
            "params_summary": "name=other-agent display_name=Other",
            "success": true,
        })];
        let results = verify_claims(&claims, &tool_calls);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], VerifyResult::Hallucination { .. }),
            "should be hallucination: tool call was for a different agent");
    }

    // ── MAX_CLAIMS_PER_RESPONSE cap ──────────────────────────

    #[test]
    fn test_claims_capped_at_max() {
        // Generate output with many claims
        let mut output = String::new();
        for i in 0..20 {
            output.push_str(&format!("成功建立 agent test-agent-{i}\n"));
        }
        let claims = extract_action_claims(&output);
        assert!(claims.len() <= MAX_CLAIMS_PER_RESPONSE,
            "claims should be capped at {MAX_CLAIMS_PER_RESPONSE}, got {}", claims.len());
    }
}
