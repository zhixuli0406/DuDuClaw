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

/// All action claim patterns, compiled once at first use.
static CLAIM_PATTERNS: LazyLock<Vec<ClaimPattern>> = LazyLock::new(|| {
    let specs: &[(&str, ClaimType)] = &[
        // ── Agent creation ───────────────────────────────────
        // Chinese
        (r"(?:已建立|建立完成|成功建立|已創建|創建完成|成功創建)\s*(?:了\s*)?(?:agent|Agent|代理)\s*[「」]?(\S+?)[「」]?", ClaimType::AgentCreated),
        (r"[「」](\S+?)[「」]\s*(?:已建立|建立完成|建立成功|已創建|創建成功)", ClaimType::AgentCreated),
        // English
        (r#"(?i)(?:created|set up|established)\s+(?:agent\s+)?["']?([a-z][a-z0-9-]{0,63})["']?\s*(?:successfully)?"#, ClaimType::AgentCreated),
        (r#"(?i)Agent\s+["']?([a-z][a-z0-9-]{0,63})["']?\s*(?:\([^)]+\)\s*)?created\s+successfully"#, ClaimType::AgentCreated),
        // ── Agent deletion ───────────────────────────────────
        (r"(?:已刪除|刪除完成|成功刪除|已移除|移除完成)\s*(?:了\s*)?(?:agent|Agent|代理)\s*[「」]?(\S+?)[「」]?", ClaimType::AgentDeleted),
        (r#"(?i)(?:deleted|removed)\s+(?:agent\s+)?["']?([a-z][a-z0-9-]{0,63})["']?"#, ClaimType::AgentDeleted),
        // ── SOUL update ──────────────────────────────────────
        (r"(?:已更新|更新完成|成功更新|已修改)\s*(?:了\s*)?(?:SOUL\.md|靈魂|人格)", ClaimType::SoulUpdated),
        (r"(?i)(?:updated|modified)\s+SOUL\.md", ClaimType::SoulUpdated),
        // ── Message sent ─────────────────────────────────────
        (r"(?:已發送|已傳送)\s*(?:訊息|消息)\s*(?:給|到)\s*[「」]?(\S+?)[「」]?", ClaimType::MessageSent),
        (r#"(?i)(?:sent|forwarded)\s+(?:message\s+)?to\s+(?:agent\s+)?["']?([a-z][a-z0-9-]{0,63})["']?"#, ClaimType::MessageSent),
        // ── Spawn ────────────────────────────────────────────
        (r"(?:已派遣|已指派|已委派)\s*(?:任務\s*)?(?:給\s*)?[「」]?(\S+?)[「」]?", ClaimType::AgentSpawned),
        (r#"(?i)(?:spawned|delegated)\s+(?:task\s+)?(?:to\s+)?(?:agent\s+)?["']?([a-z][a-z0-9-]{0,63})["']?"#, ClaimType::AgentSpawned),
    ];

    specs
        .iter()
        .filter_map(|(pat, ct)| {
            Regex::new(pat).ok().map(|re| ClaimPattern {
                re,
                claim_type: ct.clone(),
            })
        })
        .collect()
});

/// The type of action an agent claims to have performed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

    for pattern in CLAIM_PATTERNS.iter() {
        for cap in pattern.re.captures_iter(output) {
            let target_id = cap
                .get(1)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let matched_text = cap.get(0).map(|m| m.as_str().to_string()).unwrap_or_default();
            claims.push(ActionClaim {
                claim_type: pattern.claim_type.clone(),
                target_id,
                matched_text,
            });
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

    #[test]
    fn test_extract_chinese_agent_created() {
        let output = "已成功建立 agent tl-xianwen，角色為 Team Leader";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        assert!(claims.iter().any(|c| c.claim_type == ClaimType::AgentCreated));
    }

    #[test]
    fn test_extract_english_agent_created() {
        let output = "Agent 'tl-duduclaw' (Team Leader) created successfully.";
        let claims = extract_action_claims(output);
        assert!(!claims.is_empty());
        assert!(claims.iter().any(|c| c.claim_type == ClaimType::AgentCreated
            && c.target_id == "tl-duduclaw"));
    }

    #[test]
    fn test_no_claims_in_normal_text() {
        let output = "Here is the analysis of the codebase structure.";
        let claims = extract_action_claims(output);
        assert!(claims.is_empty());
    }

    #[test]
    fn test_verify_hallucination() {
        let claims = vec![ActionClaim {
            claim_type: ClaimType::AgentCreated,
            target_id: "tl-xianwen".to_string(),
            matched_text: "created agent tl-xianwen".to_string(),
        }];
        // Empty tool call log — should detect hallucination
        let results = verify_claims(&claims, &[]);
        assert_eq!(results.len(), 1);
        assert!(matches!(results[0], VerifyResult::Hallucination { .. }));
    }

    #[test]
    fn test_verify_success() {
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
}
