//! H2 — the follow-through half of the skill auto-synthesis chain.
//!
//! `gap_accumulator` fires a [`SynthesisTrigger`] and the caller marks the
//! topic pending. Until 2026-09 nothing consumed that trigger:
//! `GapAccumulator::confirm_synthesis` / `cancel_pending` and
//! `SandboxStore::add` had zero production call sites, so a triggered topic
//! stayed `pending` forever (never re-triggering) and the sandbox store was
//! permanently empty (making the whole trial-evaluation loop in
//! `channel_reply.rs` a no-op). This module is the missing consumer.
//!
//! Shape:
//!
//! ```text
//! trigger ──▶ [enabled?] ──no──▶ cancel_pending          (topic re-accumulates)
//!                │yes
//!                ▼
//!          build prompt ──▶ generate ──▶ parse ──▶ security scan
//!                │              │err        │err        │fail
//!                │              └───────────┴───────────┴──▶ cancel_pending
//!                ▼
//!      SandboxStore::add + confirm_synthesis                (trial starts)
//! ```
//!
//! Every failure path calls `cancel_pending`, never `confirm_synthesis` — a
//! failed synthesis must let the topic accumulate again rather than burn the
//! cooldown. The LLM call is injected as a closure so the orchestration is
//! testable end-to-end without a network or an account pool.
//!
//! Cost posture: gated by `agent.toml [evolution] skill_synthesis_enabled`,
//! which is **false by default**, so a stock install spends nothing here. On
//! top of that the trigger itself needs `skill_synthesis_threshold` repeated
//! gaps on the same topic and honours the per-topic synthesis cooldown.

use std::future::Future;
use std::path::Path;
use std::sync::Arc;

use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

use duduclaw_agent::registry::AgentRegistry;

use super::gap_accumulator::{GapAccumulator, SynthesisTrigger};
use super::sandbox_trial::{SandboxStore, SandboxedSkill};
use super::security_scanner::scan_skill;
use super::synthesizer::{SynthesisInput, build_synthesis_prompt, parse_synthesis_response};

/// Max tokens for one synthesis call. A SKILL.md is a short document; this is
/// a ceiling, not a target.
const SYNTHESIS_MAX_TOKENS: u32 = 2048;

/// How many recent episodic memories are offered as "successful conversation"
/// evidence. Deliberately small — the prompt already carries the gap evidence,
/// and every extra row is paid for on every synthesis.
const EVIDENCE_LIMIT: usize = 5;

/// Per-agent knobs this follow-through honours.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SynthesisSettings {
    /// `agent.toml [evolution] skill_synthesis_enabled` (default `false`).
    pub enabled: bool,
    /// `agent.toml [evolution] skill_trial_ttl` — conversations a synthesized
    /// skill gets in the sandbox before `evaluate_trial` decides its fate.
    pub trial_ttl: u32,
}

/// What happened to one triggered topic.
#[derive(Debug, Clone, PartialEq)]
pub enum FollowThrough {
    /// `skill_synthesis_enabled = false` — nothing generated, pending cleared
    /// so the topic can trigger again once the operator opts in.
    Disabled,
    /// A skill passed generation, parsing and the security scan and is now in
    /// the sandbox on trial.
    Admitted { skill_name: String },
    /// Generation, parsing or the security scan failed. Pending is cleared and
    /// no cooldown is set, so the topic re-accumulates.
    Rejected { reason: String },
}

/// Run one synthesis follow-through.
///
/// `generate` receives the fully-built (injection-hardened) prompt and returns
/// the model's raw text. Production passes `run_utility_prompt`; tests pass a
/// canned SKILL.md.
pub async fn follow_through_with<F, Fut>(
    trigger: &SynthesisTrigger,
    settings: SynthesisSettings,
    input: SynthesisInput,
    gap_accumulator: &Mutex<GapAccumulator>,
    sandbox: &Mutex<SandboxStore>,
    generate: F,
) -> FollowThrough
where
    F: FnOnce(String) -> Fut,
    Fut: Future<Output = Result<String, String>>,
{
    if !settings.enabled {
        gap_accumulator
            .lock()
            .await
            .cancel_pending(&trigger.agent_id, &trigger.topic);
        return FollowThrough::Disabled;
    }

    let prompt = build_synthesis_prompt(&input);
    let raw = match generate(prompt).await {
        Ok(text) => text,
        Err(e) => return reject(trigger, gap_accumulator, format!("generation failed: {e}")).await,
    };

    let skill = match parse_synthesis_response(&raw, trigger, &input.existing_skill_names) {
        Ok(s) => s,
        Err(e) => return reject(trigger, gap_accumulator, format!("unparseable: {e}")).await,
    };

    // The synthesized markdown is model output built from channel-derived
    // evidence — screen it with the same scanner the `skill_security_scan`
    // MCP tool uses before it can reach an agent's context. Fail closed.
    let scan = scan_skill(&skill.full_markdown, None);
    if !scan.passed {
        let categories: Vec<String> = scan
            .findings
            .iter()
            .map(|f| format!("{:?}", f.category))
            .collect();
        return reject(
            trigger,
            gap_accumulator,
            format!(
                "security scan failed ({:?}): {}",
                scan.risk_level,
                categories.join(", ")
            ),
        )
        .await;
    }

    let skill_name = skill.name.clone();
    sandbox.lock().await.add(SandboxedSkill::from_synthesized(
        skill,
        &trigger.agent_id,
        settings.trial_ttl,
    ));
    gap_accumulator
        .lock()
        .await
        .confirm_synthesis(&trigger.agent_id, &trigger.topic);
    info!(
        agent = %trigger.agent_id,
        topic = %trigger.topic,
        skill = %skill_name,
        ttl = settings.trial_ttl,
        "Skill synthesis succeeded — entered sandbox trial"
    );
    FollowThrough::Admitted { skill_name }
}

async fn reject(
    trigger: &SynthesisTrigger,
    gap_accumulator: &Mutex<GapAccumulator>,
    reason: String,
) -> FollowThrough {
    warn!(
        agent = %trigger.agent_id,
        topic = %trigger.topic,
        reason = %reason,
        "Skill synthesis rejected — topic released to re-accumulate"
    );
    gap_accumulator
        .lock()
        .await
        .cancel_pending(&trigger.agent_id, &trigger.topic);
    FollowThrough::Rejected { reason }
}

// ── Production adapter ────────────────────────────────────────────────────

/// Consume one `SynthesisTrigger` end to end with the real inputs: the agent's
/// `[evolution]` knobs, its SOUL.md, its installed skill names, recent
/// episodic evidence, and the rotated utility-model call.
///
/// Returns the outcome so tests can assert on it; the channel path fires this
/// detached and ignores the value.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    trigger: SynthesisTrigger,
    home_dir: &Path,
    agent_dir: Option<&Path>,
    agent_id: &str,
    registry: &Arc<RwLock<AgentRegistry>>,
    gap_accumulator: &Mutex<GapAccumulator>,
    sandbox: &Mutex<SandboxStore>,
) -> FollowThrough {
    let (settings, agent_soul, existing_skill_names) = {
        let reg = registry.read().await;
        match reg.get(agent_id) {
            Some(agent) => (
                SynthesisSettings {
                    enabled: agent.config.evolution.skill_synthesis_enabled,
                    trial_ttl: agent.config.evolution.skill_trial_ttl,
                },
                agent.soul.clone().unwrap_or_default(),
                agent.skills.iter().map(|s| s.name.clone()).collect(),
            ),
            None => {
                // Unknown agent ⇒ fail closed (no spend), but still release the
                // pending topic so it can accumulate again.
                debug!(
                    agent = agent_id,
                    "skill synthesis: agent not in registry — treating as disabled"
                );
                (
                    SynthesisSettings {
                        enabled: false,
                        trial_ttl: 0,
                    },
                    String::new(),
                    Vec::new(),
                )
            }
        }
    };

    let input = SynthesisInput {
        trigger: trigger.clone(),
        successful_conversations: recent_evidence(home_dir, agent_id, &trigger.topic).await,
        agent_soul,
        existing_skill_names,
    };

    let home = home_dir.to_path_buf();
    let agent_dir = agent_dir.map(Path::to_path_buf);
    let agent = agent_id.to_string();
    follow_through_with(
        &trigger,
        settings,
        input,
        gap_accumulator,
        sandbox,
        move |prompt| async move {
            crate::runtime_dispatch::run_utility_prompt(
                &home,
                agent_dir.as_deref(),
                &agent,
                "You synthesize reusable SKILL.md documents from evidence.",
                &prompt,
                SYNTHESIS_MAX_TOKENS,
            )
            .await
        },
    )
    .await
}

/// Episodic evidence for the synthesis prompt. Best-effort: any failure (no
/// memory db, open error, empty result) yields an empty list, which the prompt
/// builder renders as "No successful conversation data available."
async fn recent_evidence(home_dir: &Path, agent_id: &str, topic: &str) -> Vec<String> {
    let db_path = home_dir.join("memory.db");
    if !db_path.exists() {
        return Vec::new();
    }
    let home = home_dir.to_path_buf();
    let agent = agent_id.to_string();
    let topic = topic.to_string();
    tokio::task::spawn_blocking(move || {
        // `SqliteMemoryEngine` is !Send — built and consumed inside the closure.
        let engine = crate::memory_factory::build_memory_engine(&db_path, &home).ok()?;
        let rt = tokio::runtime::Handle::current();
        let hits = rt
            .block_on(duduclaw_core::traits::MemoryEngine::search(
                &engine,
                &agent,
                &topic,
                EVIDENCE_LIMIT,
            ))
            .ok()?;
        Some(hits.into_iter().map(|e| e.content).collect::<Vec<_>>())
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skill_lifecycle::diagnostician::SkillGap;

    fn gap(name: &str) -> SkillGap {
        SkillGap {
            suggested_name: name.to_string(),
            suggested_description: "handle refunds politely".to_string(),
            evidence: vec!["user asked about refunds again".to_string()],
        }
    }

    fn input(trigger: &SynthesisTrigger) -> SynthesisInput {
        SynthesisInput {
            trigger: trigger.clone(),
            successful_conversations: vec!["…".to_string()],
            agent_soul: "You are a support agent.".to_string(),
            existing_skill_names: vec![],
        }
    }

    const GOOD_SKILL: &str = r#"```markdown
---
name: refund-policy
description: Answer refund questions using the published policy
tags: [support, refunds]
---

# Refund policy

Quote the 14-day window and point at the policy page.
```"#;

    fn settings(enabled: bool) -> SynthesisSettings {
        SynthesisSettings {
            enabled,
            trial_ttl: 20,
        }
    }

    /// End-to-end (H2 regression): gaps accumulate → trigger fires → topic is
    /// pending → follow-through confirms → a sandbox trial actually exists.
    /// Before the fix nothing consumed the trigger, so `active_names()` was
    /// permanently empty and the topic never re-triggered.
    #[tokio::test]
    async fn gap_accumulation_to_confirmed_sandbox_trial() {
        let acc = Mutex::new(GapAccumulator::new(3, 24));
        let sandbox = Mutex::new(SandboxStore::new());

        // Three gaps on the same topic fire the trigger.
        let trigger = {
            let mut a = acc.lock().await;
            assert!(a.record_gap("agent-a", &gap("refund-policy"), 0.7).is_none());
            assert!(a.record_gap("agent-a", &gap("refund-policy"), 0.7).is_none());
            let t = a
                .record_gap("agent-a", &gap("refund-policy"), 0.7)
                .expect("threshold reached");
            a.mark_pending("agent-a", &t.topic);
            t
        };

        // While pending, further gaps are suppressed (no double synthesis).
        {
            let mut a = acc.lock().await;
            assert!(a.record_gap("agent-a", &gap("refund-policy"), 0.7).is_none());
        }
        assert!(sandbox.lock().await.active_names("agent-a").is_empty());

        let outcome = follow_through_with(
            &trigger,
            settings(true),
            input(&trigger),
            &acc,
            &sandbox,
            |_prompt| async { Ok(GOOD_SKILL.to_string()) },
        )
        .await;

        assert_eq!(
            outcome,
            FollowThrough::Admitted {
                skill_name: "refund-policy".to_string()
            }
        );
        assert_eq!(
            sandbox.lock().await.active_names("agent-a"),
            vec!["refund-policy".to_string()],
            "SandboxStore::add must have a live call site"
        );
        let sandboxed = {
            let s = sandbox.lock().await;
            s.get("agent-a", "refund-policy").cloned().unwrap()
        };
        assert_eq!(sandboxed.ttl_conversations, 20);

        // confirm_synthesis cleared pending AND set the cooldown, so the same
        // topic does not immediately re-trigger.
        {
            let mut a = acc.lock().await;
            assert!(a.record_gap("agent-a", &gap("refund-policy"), 0.7).is_none());
        }
    }

    /// A failed generation releases the topic instead of leaving it stuck
    /// `pending` (the exact defect H2 describes).
    #[tokio::test]
    async fn generation_failure_releases_pending_topic() {
        let acc = Mutex::new(GapAccumulator::new(2, 24));
        let sandbox = Mutex::new(SandboxStore::new());
        let trigger = {
            let mut a = acc.lock().await;
            a.record_gap("agent-b", &gap("refund-policy"), 0.7);
            let t = a.record_gap("agent-b", &gap("refund-policy"), 0.7).unwrap();
            a.mark_pending("agent-b", &t.topic);
            t
        };

        let outcome = follow_through_with(
            &trigger,
            settings(true),
            input(&trigger),
            &acc,
            &sandbox,
            |_prompt| async { Err("rate limited".to_string()) },
        )
        .await;
        assert!(matches!(outcome, FollowThrough::Rejected { .. }));
        assert!(sandbox.lock().await.active_names("agent-b").is_empty());

        // No cooldown was set, so the very next gap re-triggers.
        let mut a = acc.lock().await;
        assert!(
            a.record_gap("agent-b", &gap("refund-policy"), 0.7).is_some(),
            "a failed synthesis must let the topic trigger again"
        );
    }

    /// Unparseable model output is a rejection, never an admission.
    #[tokio::test]
    async fn unparseable_response_is_rejected() {
        let acc = Mutex::new(GapAccumulator::new(1, 24));
        let sandbox = Mutex::new(SandboxStore::new());
        let trigger = {
            let mut a = acc.lock().await;
            let t = a.record_gap("agent-c", &gap("refund-policy"), 0.7).unwrap();
            a.mark_pending("agent-c", &t.topic);
            t
        };
        let outcome = follow_through_with(
            &trigger,
            settings(true),
            input(&trigger),
            &acc,
            &sandbox,
            |_prompt| async { Ok("I could not write a skill, sorry.".to_string()) },
        )
        .await;
        assert!(matches!(outcome, FollowThrough::Rejected { .. }));
        assert!(sandbox.lock().await.active_names("agent-c").is_empty());
    }

    /// The knob is fail-closed: default-off spends nothing and admits nothing.
    #[tokio::test]
    async fn disabled_knob_spends_nothing() {
        let acc = Mutex::new(GapAccumulator::new(1, 24));
        let sandbox = Mutex::new(SandboxStore::new());
        let trigger = {
            let mut a = acc.lock().await;
            let t = a.record_gap("agent-d", &gap("refund-policy"), 0.7).unwrap();
            a.mark_pending("agent-d", &t.topic);
            t
        };
        let outcome = follow_through_with(
            &trigger,
            settings(false),
            input(&trigger),
            &acc,
            &sandbox,
            |_prompt| async { panic!("must not call the model when disabled") },
        )
        .await;
        assert_eq!(outcome, FollowThrough::Disabled);
        assert!(sandbox.lock().await.active_names("agent-d").is_empty());
    }
}
