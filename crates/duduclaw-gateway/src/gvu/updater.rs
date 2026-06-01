//! Updater — applies verified proposals with versioning and rollback capability.
//!
//! After a proposal passes verification:
//! 1. Read current SOUL.md → compute rollback content
//! 2. Apply changes → write new SOUL.md
//! 3. Update soul_guard hash (accept_soul_change)
//! 4. Record version in VersionStore with observation period
//!
//! After observation period ends:
//! 5. Collect post-metrics → compare with pre-metrics
//! 6. Confirm or rollback based on tolerance thresholds

use std::path::Path;

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use unicode_normalization::UnicodeNormalization;

use super::proposal::{EvolutionProposal, SoulPatch, SoulPatchOp};
use super::version_store::{SoulVersion, VersionMetrics, VersionStatus, VersionStore};

/// Default observation period: 24 hours.
const DEFAULT_OBSERVATION_HOURS: f64 = 24.0;

/// Hard cap on the line count of SOUL.md after applying a proposal.
///
/// The pre-2026-05-17 updater appended the LLM's entire Markdown narrative
/// to SOUL.md unconditionally. Over multiple GVU cycles that ballooned
/// agnes/SOUL.md from 61 → 592 lines of mostly meta-discussion (proposal
/// diagnostics, rationales, expected_improvement tables). The cap is the
/// floor of defense: even with [`strip_proposal_meta`] cleaning the input,
/// SOUL.md is not allowed to grow without bound.
///
/// 150 is generous — a hand-authored SOUL.md is typically 30–80 lines,
/// so this leaves room for ~10 cumulative evolution increments before
/// operator intervention is required.
pub(crate) const SOUL_MAX_LINES: usize = 150;

/// Hard cap on the byte size of SOUL.md after applying a proposal.
///
/// Belt-and-braces with [`SOUL_MAX_LINES`] — a proposal that smuggles in
/// very long lines (e.g. a single-line table or base64 blob) would slip
/// past the line cap but blow the prompt budget.
pub(crate) const SOUL_MAX_BYTES: usize = 8 * 1024;

/// Markdown headers (case-insensitive) that signal a proposal-meta section
/// the LLM emits as part of its reasoning but which has no business being
/// written into SOUL.md. When [`strip_proposal_meta`] sees one of these as
/// an h1/h2/h3 header, it drops the section and everything beneath it
/// until the next h1/h2 header of equal or higher precedence.
const META_SECTION_HEADERS: &[&str] = &[
    // English
    "analysis",
    "diagnosis",
    "rationale",
    "expected_improvement",
    "expected improvement",
    "wiki_proposals",
    "wiki proposals",
    "implementation note",
    // Chinese
    "診斷",
    "分析",
    "提議修改",
    "提案",
    "預期改善",
    "預期改進",
    "expected_improvement", // duplicate for safety
    // YAML/JSON keys the LLM sometimes Markdown-fies
    "proposed_changes",
];

/// Strip proposal-meta sections from LLM-generated patch content.
///
/// The Generator's `proposed_changes` field often contains a mix of
/// (a) the actual SOUL.md text to apply and
/// (b) meta-discussion about *why* the change is being proposed.
/// Categories in (b) historically include `## 診斷`, `## proposed_changes`,
/// `## rationale`, `## expected_improvement`, `## wiki_proposals`, and the
/// freeform `## Analysis` section. None of these belong in SOUL.md — they're
/// the GVU loop's internal reasoning, not behavioral instructions for the
/// agent.
///
/// This filter drops a header line (h1/h2/h3) and every line that follows
/// until the next h1/h2 header of equal-or-higher precedence. It is
/// deliberately conservative: it keeps any content above the first meta
/// header and any non-meta section after a meta section ends.
pub(crate) fn strip_proposal_meta(content: &str) -> String {
    let mut out_lines: Vec<&str> = Vec::with_capacity(content.lines().count());
    let mut suppress_until_top_level = false;

    for line in content.lines() {
        let trimmed = line.trim_start();

        // h1 always resets suppression — a new top-level section starts.
        let is_h1 = trimmed.starts_with("# ") && !trimmed.starts_with("## ");
        // h2/h3 we examine to decide whether they're meta.
        let is_h2 = trimmed.starts_with("## ") && !trimmed.starts_with("### ");
        let is_h3 = trimmed.starts_with("### ");

        if is_h1 {
            // Always re-evaluate at h1 boundaries.
            suppress_until_top_level = is_meta_header(trimmed);
            if !suppress_until_top_level {
                out_lines.push(line);
            }
            continue;
        }

        if is_h2 {
            suppress_until_top_level = is_meta_header(trimmed);
            if !suppress_until_top_level {
                out_lines.push(line);
            }
            continue;
        }

        if is_h3 && is_meta_header(trimmed) {
            // h3 meta — drop just this one section until the next h2/h1 or h3
            suppress_until_top_level = true;
            continue;
        }

        if !suppress_until_top_level {
            out_lines.push(line);
        }
    }

    // Re-join and tidy: collapse 3+ consecutive blank lines into 1.
    let joined = out_lines.join("\n");
    collapse_blank_lines(&joined)
}

fn is_meta_header(header_line: &str) -> bool {
    // Strip leading #'s and whitespace to get the title text.
    let title = header_line.trim_start_matches('#').trim();
    // Many headers look like "## proposed_changes" or "### rationale"; also tolerate
    // "## Proposed Changes:" or surrounding emoji.
    let title_lower = title.to_lowercase();
    META_SECTION_HEADERS.iter().any(|m| {
        let m_lower = m.to_lowercase();
        title_lower == m_lower
            || title_lower.starts_with(&format!("{m_lower}:"))
            || title_lower.starts_with(&format!("{m_lower} "))
    })
}

/// Apply a structured [`SoulPatch`] to a SOUL.md string and return the new content.
///
/// Locates the target section by matching the `## <title>` (h2) header line.
/// Section matching is case-sensitive and ignores leading/trailing whitespace.
/// For `AddSection` the title may name a non-existent section — a new h2
/// section is appended to the document.
///
/// This is the typed alternative to the legacy append flow in [`Updater::apply`].
/// Compared to free-form append it:
/// - Does not duplicate content the LLM didn't ask to duplicate
/// - Cannot leak proposal-meta narrative into SOUL.md
/// - Has bounded growth by construction (only the targeted section changes)
pub fn apply_patch_to_soul(current: &str, patch: &SoulPatch) -> Result<String, String> {
    let section = patch.section.trim();
    if section.is_empty() {
        return Err("SoulPatch.section is empty".to_string());
    }
    // Defence: section name must not contain newlines or markdown header tokens —
    // an attacker could otherwise craft a section name that smuggles arbitrary
    // content into SOUL.md.
    if section.contains('\n') || section.contains("##") {
        return Err("SoulPatch.section contains forbidden characters".to_string());
    }
    if patch.content.len() > 4 * 1024 {
        return Err("SoulPatch.content exceeds 4KB per-patch budget".to_string());
    }

    let lines: Vec<&str> = current.lines().collect();
    let target_header = format!("## {section}");

    // Locate the section's header line and the line index *after* its body.
    let mut header_idx: Option<usize> = None;
    for (i, l) in lines.iter().enumerate() {
        if l.trim_end() == target_header {
            header_idx = Some(i);
            break;
        }
    }

    let new_content = match (header_idx, &patch.op) {
        (None, SoulPatchOp::AddSection) => {
            // Append new section at the end.
            let mut out = current.trim_end().to_string();
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(&target_header);
            out.push('\n');
            out.push('\n');
            out.push_str(patch.content.trim_end());
            out.push('\n');
            out
        }
        (None, _) => {
            return Err(format!(
                "SoulPatch target section '{section}' not found in SOUL.md"
            ));
        }
        (Some(h), op) => {
            // Find the next h1/h2 line (exclusive) to delimit the section body.
            let mut next_section_idx = lines.len();
            for (j, l) in lines.iter().enumerate().skip(h + 1) {
                let t = l.trim_start();
                if t.starts_with("## ") || (t.starts_with("# ") && !t.starts_with("## ")) {
                    next_section_idx = j;
                    break;
                }
            }

            // The body lines are [h+1 .. next_section_idx).
            let header_line = lines[h];
            let body: Vec<&str> = lines[h + 1..next_section_idx].to_vec();
            let after: Vec<&str> = lines[next_section_idx..].to_vec();
            let before: Vec<&str> = lines[..h].to_vec();

            let new_body: String = match op {
                SoulPatchOp::Replace => {
                    let mut b = String::new();
                    b.push('\n');
                    b.push_str(patch.content.trim_end());
                    b.push('\n');
                    b
                }
                SoulPatchOp::AppendWithin => {
                    let existing = body.join("\n");
                    let existing_trimmed = existing.trim_end();
                    let mut b = String::new();
                    if !existing_trimmed.is_empty() {
                        b.push_str(existing_trimmed);
                        b.push_str("\n\n");
                    } else {
                        b.push('\n');
                    }
                    b.push_str(patch.content.trim_end());
                    b.push('\n');
                    // Re-prepend the leading blank line if there was one.
                    if body.first().map_or(false, |l| l.trim().is_empty()) {
                        format!("\n{b}")
                    } else {
                        b
                    }
                }
                SoulPatchOp::PrependWithin => {
                    let mut b = String::new();
                    b.push('\n');
                    b.push_str(patch.content.trim_end());
                    let existing = body.join("\n");
                    let existing_trimmed = existing.trim_start_matches('\n');
                    if !existing_trimmed.is_empty() {
                        b.push_str("\n\n");
                        b.push_str(existing_trimmed.trim_end());
                        b.push('\n');
                    } else {
                        b.push('\n');
                    }
                    b
                }
                SoulPatchOp::AddSection => {
                    // Section already exists — fall back to Replace semantics
                    // (or arguably error). Treating as Replace is more forgiving
                    // for LLMs that misclassify.
                    let mut b = String::new();
                    b.push('\n');
                    b.push_str(patch.content.trim_end());
                    b.push('\n');
                    b
                }
                SoulPatchOp::Consolidate => {
                    // Hard contract: new content must be shorter than the body
                    // being replaced. Reject misclassified patches where the
                    // LLM tagged the op as Consolidate but actually grew the
                    // section — that is exactly the failure mode this op
                    // exists to prevent.
                    let existing_body = body.join("\n");
                    let existing_trimmed = existing_body.trim();
                    let new_trimmed = patch.content.trim();
                    if existing_trimmed.is_empty() {
                        return Err(
                            "Consolidate target section has no body to compress".to_string(),
                        );
                    }
                    if new_trimmed.len() >= existing_trimmed.len() {
                        return Err(format!(
                            "Consolidate must shrink the section — new content is {} bytes \
                             but existing body is {} bytes",
                            new_trimmed.len(),
                            existing_trimmed.len(),
                        ));
                    }
                    let mut b = String::new();
                    b.push('\n');
                    b.push_str(new_trimmed);
                    b.push('\n');
                    b
                }
            };

            let mut out = String::new();
            if !before.is_empty() {
                out.push_str(&before.join("\n"));
                out.push('\n');
            }
            out.push_str(header_line);
            out.push_str(new_body.trim_end());
            out.push('\n');
            if !after.is_empty() {
                out.push('\n');
                out.push_str(&after.join("\n"));
            }
            out.trim_end().to_string() + "\n"
        }
    };

    Ok(new_content)
}

fn collapse_blank_lines(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut blank_run = 0usize;
    for line in s.lines() {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 1 {
                out.push('\n');
            }
        } else {
            blank_run = 0;
            out.push_str(line);
            out.push('\n');
        }
    }
    // Drop trailing newline that the loop adds at the very end.
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Outcome of judging an observation period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OutcomeVerdict {
    /// Metrics within tolerance — confirm the change.
    Confirm,
    /// Metrics degraded — rollback.
    Rollback { reason: String },
    /// Not enough data — extend observation.
    ExtendObservation { extra_hours: f64 },
}

/// Updater applies proposals and manages the observation lifecycle.
pub struct Updater {
    version_store: VersionStore,
    observation_hours: f64,
}

impl Updater {
    pub fn new(version_store: VersionStore, observation_hours: Option<f64>) -> Self {
        Self {
            version_store,
            observation_hours: observation_hours.unwrap_or(DEFAULT_OBSERVATION_HOURS),
        }
    }

    /// Access the version store (for heartbeat observation checking).
    pub fn version_store(&self) -> &VersionStore {
        &self.version_store
    }

    /// Apply a verified proposal to SOUL.md.
    ///
    /// Returns the created SoulVersion for tracking.
    pub fn apply(
        &self,
        proposal: &EvolutionProposal,
        agent_dir: &Path,
        pre_metrics: VersionMetrics,
    ) -> Result<SoulVersion, String> {
        let soul_path = agent_dir.join("SOUL.md");

        // Scan proposal content for prompt injection before applying to SOUL.md.
        let scan = duduclaw_security::input_guard::scan_input(
            &proposal.content,
            duduclaw_security::input_guard::DEFAULT_BLOCK_THRESHOLD,
        );
        if scan.blocked {
            warn!(
                agent = %proposal.agent_id,
                score = scan.risk_score,
                rules = ?scan.matched_rules,
                "GVU proposal blocked by content safety scan"
            );
            return Err(format!(
                "GVU proposal contains unsafe content (score {}): {:?}",
                scan.risk_score, scan.matched_rules
            ));
        }

        // Scan proposal for hidden/malicious Markdown content (Soul-Evil Attack defense).
        let soul_scan = duduclaw_security::soul_scanner::scan_soul(&proposal.content);
        if !soul_scan.clean {
            let max_severity = soul_scan.findings.iter().map(|f| f.severity).max().unwrap_or(0);
            if max_severity >= 70 {
                warn!(
                    agent = %proposal.agent_id,
                    threat_score = soul_scan.threat_score,
                    findings = soul_scan.findings.len(),
                    "GVU proposal blocked by SOUL.md scanner"
                );
                return Err(format!(
                    "GVU proposal contains hidden content (threat score {}/100): {}",
                    soul_scan.threat_score, soul_scan.summary,
                ));
            }
            // Low-severity findings: log but allow (e.g., short HTML comments)
            info!(
                agent = %proposal.agent_id,
                threat_score = soul_scan.threat_score,
                "GVU proposal has low-severity SOUL.md scanner findings — allowing"
            );
        }

        // Verify the proposal does not attempt to override behavioral contracts.
        // NFKC-normalize and strip invisible characters before checking to prevent
        // Unicode fullwidth/homoglyph bypass attacks (R4-H2).
        let normalized: String = proposal.content.nfkc().collect();
        let clean: String = normalized
            .chars()
            .filter(|c| !matches!(*c,
                '\u{00AD}' | '\u{200B}'..='\u{200F}' | '\u{FEFF}' | '\u{00A0}'
            ))
            .collect();
        let lower = clean.to_lowercase();
        if lower.contains("must_not") || lower.contains("must_always") || lower.contains("contract.toml") {
            return Err("GVU proposal cannot modify behavioral contracts".to_string());
        }

        // Read current SOUL.md (for rollback)
        let current_content = std::fs::read_to_string(&soul_path)
            .map_err(|e| format!("Failed to read SOUL.md: {e}"))?;

        // Prefer the structured patch path when the Generator emitted one.
        // The legacy free-form append remains as a fallback so older LLM outputs
        // (and existing in-flight proposals) continue to apply.
        let new_content = if let Some(patch) = &proposal.patch {
            apply_patch_to_soul(&current_content, patch)
                .map_err(|e| format!("Structured patch failed: {e}"))?
        } else {
            // Strip LLM proposal-meta sections (diagnosis, rationale, expected_improvement,
            // wiki_proposals, etc.) before appending. Without this, every GVU cycle would
            // append the model's full reasoning narrative — leading to the agnes/SOUL.md
            // bloat (61 → 592 lines over 5 cycles) observed in production on 2026-05-17.
            let sanitized = strip_proposal_meta(&proposal.content);
            let sanitized_trimmed = sanitized.trim();
            if sanitized_trimmed.is_empty() {
                return Err(
                    "GVU proposal contained only meta-discussion (diagnosis / rationale / \
                     expected_improvement) — no behavioral changes to apply"
                        .to_string(),
                );
            }

            // Build new SOUL.md content by appending the proposed changes.
            // Always append rather than replace — this prevents a malicious or
            // broken LLM output from wiping out the entire SOUL.md.
            format!(
                "{}\n\n<!-- Evolution update ({}) -->\n{}",
                current_content,
                Utc::now().format("%Y-%m-%d"),
                sanitized_trimmed,
            )
        };

        // Validate new content
        if new_content.trim().is_empty() {
            return Err("Resulting SOUL.md would be empty".to_string());
        }
        if new_content.len() > 50_000 {
            return Err("Resulting SOUL.md exceeds 50KB limit".to_string());
        }

        // Hard caps on growth — independent of the 50KB ceiling above and of ASI.
        // These prevent the slow-bloat failure mode (each individual append passes
        // ASI because the baseline grows with it, but the absolute size eventually
        // blows the agent's prompt budget).
        let projected_lines = new_content.lines().count();
        if projected_lines > SOUL_MAX_LINES {
            warn!(
                agent = %proposal.agent_id,
                projected_lines,
                cap = SOUL_MAX_LINES,
                "GVU proposal rejected: SOUL.md line-count cap exceeded"
            );
            return Err(format!(
                "Applying this proposal would push SOUL.md to {projected_lines} lines, \
                 exceeding the {SOUL_MAX_LINES}-line cap. Manual review required — \
                 SOUL.md may have accumulated stale evolution increments and need consolidation."
            ));
        }
        if new_content.len() > SOUL_MAX_BYTES {
            warn!(
                agent = %proposal.agent_id,
                projected_bytes = new_content.len(),
                cap = SOUL_MAX_BYTES,
                "GVU proposal rejected: SOUL.md byte-size cap exceeded"
            );
            return Err(format!(
                "Applying this proposal would push SOUL.md to {} bytes, exceeding the \
                 {SOUL_MAX_BYTES}-byte cap. Manual review required.",
                new_content.len()
            ));
        }

        // Compute Agent Stability Index (ASI) — reject if drift is too extreme.
        //
        // Pick a config sized to the baseline: when SOUL.md is tiny (< ~1 KB),
        // an append is proportionally huge and the default content-weighted
        // threshold is permanently tripped. `for_baseline_size` falls back to
        // the strict default once the baseline grows past the bootstrap size.
        let asi_config = duduclaw_security::stability_index::AsiConfig::for_baseline_size(
            current_content.len(),
        );
        let asi = duduclaw_security::stability_index::compute_asi(
            &current_content,
            &new_content,
            &[], // Version distances populated by heartbeat, not available inline
            &asi_config,
        );
        if asi.level == duduclaw_security::stability_index::AsiLevel::Critical {
            warn!(
                agent = %proposal.agent_id,
                asi = asi.index,
                summary = %asi.summary,
                "GVU proposal rejected: ASI below critical threshold"
            );
            return Err(format!(
                "GVU proposal would cause critical identity drift (ASI={:.3}): {}",
                asi.index, asi.summary,
            ));
        }
        if asi.level == duduclaw_security::stability_index::AsiLevel::Warning {
            warn!(
                agent = %proposal.agent_id,
                asi = asi.index,
                summary = %asi.summary,
                "GVU proposal triggers ASI warning — proceeding with caution"
            );
        }

        // Atomic write: write to temp file, then rename (prevents truncated SOUL.md on crash)
        let tmp_path = soul_path.with_extension("md.gvu_tmp");
        std::fs::write(&tmp_path, &new_content)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to write temp SOUL.md: {e}")
            })?;

        // Record version BEFORE rename — if this fails, temp file is cleaned up and SOUL.md is untouched
        // (version recording moved here, before the actual file swap)

        // Compute hash of new content
        let soul_hash = {
            use ring::digest;
            let d = digest::digest(&digest::SHA256, new_content.as_bytes());
            d.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>()
        };

        let now = Utc::now();
        let observation_end = now + chrono::Duration::seconds((self.observation_hours * 3600.0) as i64);

        // Compute SHA-256 hash of rollback content for integrity verification on rollback
        let rollback_diff_hash = {
            use ring::digest;
            let d = digest::digest(&digest::SHA256, current_content.as_bytes());
            d.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>()
        };

        let version = SoulVersion {
            version_id: uuid::Uuid::new_v4().to_string(),
            agent_id: proposal.agent_id.clone(),
            soul_hash,
            soul_summary: new_content.chars().take(200).collect(),
            applied_at: now,
            observation_end,
            status: VersionStatus::Observing,
            pre_metrics,
            post_metrics: None,
            proposal_id: proposal.id.clone(),
            rollback_diff: current_content,
            rollback_diff_hash: Some(rollback_diff_hash),
        };

        // Step 1: Record version to SQLite (if this fails, SOUL.md is untouched)
        if let Err(e) = self.version_store.record_version(&version) {
            let _ = std::fs::remove_file(&tmp_path);
            return Err(format!("Failed to record version: {e}"));
        }

        // Step 2: Atomic rename (if this fails, version record exists but SOUL.md unchanged — safe)
        if let Err(e) = std::fs::rename(&tmp_path, &soul_path) {
            let _ = std::fs::remove_file(&tmp_path);
            let _ = self.version_store.mark_rolled_back(&version.version_id, "rename failed");
            return Err(format!("Failed to rename SOUL.md: {e}"));
        }

        // Step 3: Update soul_guard hash (if this fails, next heartbeat detects drift — recoverable)
        if let Err(e) = duduclaw_security::soul_guard::accept_soul_change(&proposal.agent_id, agent_dir) {
            warn!(
                agent = %proposal.agent_id,
                "Failed to update soul hash after apply: {e} — soul_guard will detect drift on next heartbeat"
            );
        }

        info!(
            agent = %proposal.agent_id,
            version = %version.version_id,
            observation_end = %observation_end.to_rfc3339(),
            "SOUL.md updated atomically, observation period started"
        );

        Ok(version)
    }

    /// After this much wall-clock time since `applied_at`, an observation
    /// with `< 5` conversations is auto-confirmed instead of extended.
    ///
    /// #10 (2026-05-11) — prevents the infinite-extend loop that would
    /// otherwise pin a SOUL version in `observing` forever. Sub-agents
    /// without their own channel (e.g. `duduclaw-tl`) only get traffic when
    /// another agent spawns them. If nobody spawns one for a week, the
    /// `< 5 conversations → extend` rule would keep cycling every 12 h
    /// without progress — and crucially, every extend keeps the
    /// `already_observing` guard active, blocking the *next* GVU proposal
    /// for that agent. Empirical evidence (5/11 12:33Z): duduclaw-tl
    /// reached its 2nd extend with 0 conversations — without this cap it
    /// was on track for an infinite loop.
    ///
    /// 72 h is the chosen ceiling because:
    /// - 24 h initial window + 2× 12 h extends = 48 h, comfortably under cap
    /// - 72 h still gives a slow-burn cron task three nightly fires
    /// - Auto-confirm is safe: the new version was already L1+L3 verified
    ///   before applying, and "nobody used it" is not a signal it's bad.
    pub(crate) const MAX_OBSERVATION_HOURS_WITHOUT_DATA: i64 = 72;

    /// Judge whether an observation period passed or failed.
    ///
    /// Pure wrapper around [`Self::judge_outcome_at`] using `Utc::now()`.
    /// Production code path; tests use the clock-injecting variant below.
    pub fn judge_outcome(
        &self,
        version: &SoulVersion,
        post_metrics: &VersionMetrics,
    ) -> OutcomeVerdict {
        self.judge_outcome_at(version, post_metrics, Utc::now())
    }

    /// Clock-injecting variant of [`Self::judge_outcome`]. The `now`
    /// parameter exists so tests can drive the time-based cap (#10)
    /// without sleeping or freezing the system clock.
    pub fn judge_outcome_at(
        &self,
        version: &SoulVersion,
        post_metrics: &VersionMetrics,
        now: chrono::DateTime<Utc>,
    ) -> OutcomeVerdict {
        // Not enough data → extend, UNLESS we've been waiting too long.
        if post_metrics.conversations_count < 5 {
            // #10 cap: a SOUL.md observation cannot stay in observing
            // forever just because no traffic arrived. After
            // MAX_OBSERVATION_HOURS_WITHOUT_DATA, auto-confirm so the
            // GVU loop can run again on the next silence breaker.
            let elapsed_hours = (now - version.applied_at).num_hours();
            if elapsed_hours >= Self::MAX_OBSERVATION_HOURS_WITHOUT_DATA {
                info!(
                    agent = %version.agent_id,
                    version = %version.version_id,
                    elapsed_hours,
                    conversations = post_metrics.conversations_count,
                    "Observation auto-confirmed: no-traffic timeout exceeded (#10)"
                );
                return OutcomeVerdict::Confirm;
            }
            return OutcomeVerdict::ExtendObservation { extra_hours: 12.0 };
        }

        let pre = &version.pre_metrics;

        // Check feedback ratio: tolerate 3% dip
        let feedback_delta = post_metrics.positive_feedback_ratio - pre.positive_feedback_ratio;
        if feedback_delta < -0.03 && pre.positive_feedback_ratio > 0.0 {
            return OutcomeVerdict::Rollback {
                reason: format!(
                    "Feedback ratio dropped {:.1}% (from {:.2} to {:.2})",
                    feedback_delta.abs() * 100.0,
                    pre.positive_feedback_ratio,
                    post_metrics.positive_feedback_ratio,
                ),
            };
        }

        // Check prediction error: tolerate 5% increase
        let error_delta = post_metrics.avg_prediction_error - pre.avg_prediction_error;
        if error_delta > 0.05 && pre.avg_prediction_error > 0.0 {
            return OutcomeVerdict::Rollback {
                reason: format!(
                    "Prediction error increased {:.1}% (from {:.3} to {:.3})",
                    error_delta * 100.0,
                    pre.avg_prediction_error,
                    post_metrics.avg_prediction_error,
                ),
            };
        }

        // Check contract violations: must not increase
        if post_metrics.contract_violations > pre.contract_violations {
            return OutcomeVerdict::Rollback {
                reason: format!(
                    "Contract violations increased from {} to {}",
                    pre.contract_violations, post_metrics.contract_violations,
                ),
            };
        }

        OutcomeVerdict::Confirm
    }

    /// Execute a rollback: restore SOUL.md from the version's stored content.
    pub fn execute_rollback(
        &self,
        version: &SoulVersion,
        agent_dir: &Path,
    ) -> Result<(), String> {
        let soul_path = agent_dir.join("SOUL.md");

        // Verify rollback_diff integrity before writing
        if let Some(ref expected_hash) = version.rollback_diff_hash {
            use ring::digest;
            let actual = {
                let d = digest::digest(&digest::SHA256, version.rollback_diff.as_bytes());
                d.as_ref().iter().map(|b| format!("{b:02x}")).collect::<String>()
            };
            if actual != *expected_hash {
                return Err("Rollback diff integrity check failed: hash mismatch".to_string());
            }
        }

        // Atomic rollback: write to temp file, then rename (same pattern as apply)
        let tmp_path = soul_path.with_extension("md.rollback_tmp");
        std::fs::write(&tmp_path, &version.rollback_diff)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to write rollback tmp: {e}")
            })?;
        std::fs::rename(&tmp_path, &soul_path)
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp_path);
                format!("Failed to rename rollback: {e}")
            })?;

        // Update soul_guard hash
        if let Err(e) = duduclaw_security::soul_guard::accept_soul_change(&version.agent_id, agent_dir) {
            warn!(agent = %version.agent_id, "Failed to update soul hash after rollback: {e}");
        }

        self.version_store.mark_rolled_back(&version.version_id, "observation_failed")?;

        warn!(
            agent = %version.agent_id,
            version = %version.version_id,
            "SOUL.md rolled back to previous version"
        );

        Ok(())
    }

    /// Confirm a version: mark it as permanent.
    pub fn execute_confirm(
        &self,
        version: &SoulVersion,
        post_metrics: &VersionMetrics,
    ) -> Result<(), String> {
        self.version_store.mark_confirmed(&version.version_id, post_metrics)?;

        info!(
            agent = %version.agent_id,
            version = %version.version_id,
            "SOUL.md version confirmed as permanent"
        );

        Ok(())
    }
}
