//! Shared, deterministic CCR paired-task and recorded-evidence evaluator.
//!
//! The CLI and admin HTTP route call the same `evaluate_replay_bytes` entry
//! point. Uploaded evidence is only held in memory during the request.

use std::collections::{BTreeMap, BTreeSet};

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_llm::{
    CCR_FIND_TOOL, CCR_RETRIEVE_TOOL, ChatResponse, NormalizedUsage, StopReason, ToolLoopTelemetry,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub const MAX_REPLAY_INPUT_BYTES: usize = 16_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Arm {
    Raw,
    Lossless,
    Lossy,
    Ccr,
}

impl Arm {
    pub const ALL: [Self; 4] = [Self::Raw, Self::Lossless, Self::Lossy, Self::Ccr];
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub task_id: String,
    pub arm: Arm,
    pub conditions: EvaluationConditions,
    pub success: bool,
    /// Supplied cost for the complete run, including every provider round.
    /// Absent for a replay without independently checked billing receipts.
    #[serde(default)]
    pub billed_cost_millicents: Option<u64>,
    pub telemetry: ToolLoopTelemetry,
}

/// Digests of the exact inputs and execution policy held fixed across arms.
/// The comparator checks equality; it does not attest how callers computed them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EvaluationConditions {
    pub task_input_sha256: String,
    pub source_set_sha256: String,
    pub model_settings_sha256: String,
    pub authorization_sha256: String,
}

impl EvaluationConditions {
    fn valid(&self) -> bool {
        [
            &self.task_input_sha256,
            &self.source_set_sha256,
            &self.model_settings_sha256,
            &self.authorization_sha256,
        ]
        .into_iter()
        .all(|digest| {
            digest.len() == 64
                && digest
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    /// `synthetic` or `user_supplied_billing`; echoed without attestation.
    pub cost_source: String,
    pub observations: Vec<Observation>,
}

#[derive(Debug, Serialize)]
pub struct ArmReport {
    pub arm: Arm,
    pub tasks: usize,
    pub successes: usize,
    pub success_rate: f64,
    pub total_cost_millicents: Option<u64>,
    pub cost_per_success_millicents: Option<f64>,
    pub p95_latency_millis: u128,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_input_ratio: Option<f64>,
    pub usage_complete_tasks: usize,
    pub retrieval_attempts: u64,
    pub retrieval_misses: u64,
    pub retrieval_miss_rate: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: &'static str,
    pub cost_source: String,
    pub paired_tasks: usize,
    pub arms: Vec<ArmReport>,
    /// Positive means CCR improves on raw for the same set of tasks.
    pub ccr_success_delta_vs_raw: f64,
    /// Negative means CCR costs less per successful task.
    pub ccr_cost_per_success_delta_vs_raw_millicents: Option<f64>,
    pub limitations: Vec<&'static str>,
}

pub fn error(message: impl std::fmt::Display) -> DuDuClawError {
    DuDuClawError::Gateway(format!("CCR task comparison: {message}"))
}

fn checked_sum(mut values: impl Iterator<Item = u64>, label: &str) -> Result<u64> {
    values.try_fold(0_u64, |sum, value| {
        sum.checked_add(value)
            .ok_or_else(|| error(format!("{label} overflow")))
    })
}

fn ratio(numerator: u64, denominator: u64) -> Option<f64> {
    (denominator > 0).then(|| numerator as f64 / denominator as f64)
}

fn p95(values: &mut [u128]) -> u128 {
    values.sort_unstable();
    values[(95 * values.len()).div_ceil(100) - 1]
}

pub fn summarize(input: Input) -> Result<Report> {
    if !matches!(
        input.cost_source.as_str(),
        "synthetic" | "user_supplied_billing" | "unbilled_replay"
    ) {
        return Err(error(
            "cost_source must be synthetic, user_supplied_billing, or unbilled_replay",
        ));
    }
    if input.observations.is_empty() || input.observations.len() > 40_000 {
        return Err(error("expected 1 to 40000 observations"));
    }
    let unbilled = input.cost_source == "unbilled_replay";
    if input
        .observations
        .iter()
        .any(|row| row.billed_cost_millicents.is_none() != unbilled)
    {
        return Err(error(
            "billing must be present for every arm or absent for every unbilled replay arm",
        ));
    }
    let mut by_task: BTreeMap<&str, (EvaluationConditions, BTreeSet<Arm>)> = BTreeMap::new();
    for item in &input.observations {
        if item.task_id.is_empty()
            || item.task_id.len() > 128
            || item.task_id.chars().any(char::is_control)
        {
            return Err(error("task_id must be 1 to 128 printable characters"));
        }
        if !item.conditions.valid() {
            return Err(error(format!(
                "invalid condition digest for {}",
                item.task_id
            )));
        }
        let t = &item.telemetry;
        if t.provider_rounds == 0 || t.usage_reported_rounds > t.provider_rounds {
            return Err(error(format!(
                "invalid provider rounds for {}",
                item.task_id
            )));
        }
        if t.ccr_find_hits.checked_add(t.ccr_find_misses) != Some(t.ccr_find_attempts)
            || t.ccr_retrieve_successes.checked_add(t.ccr_retrieve_misses)
                != Some(t.ccr_retrieve_attempts)
        {
            return Err(error(format!("invalid CCR counters for {}", item.task_id)));
        }
        if item.arm != Arm::Ccr
            && (t.ccr_compressed_results > 0
                || t.ccr_find_attempts > 0
                || t.ccr_retrieve_attempts > 0)
        {
            return Err(error(format!(
                "CCR activity in non-CCR arm for {}",
                item.task_id
            )));
        }
        let (conditions, arms) = by_task
            .entry(&item.task_id)
            .or_insert_with(|| (item.conditions.clone(), BTreeSet::new()));
        if conditions != &item.conditions {
            return Err(error(format!(
                "evaluation conditions differ for {}",
                item.task_id
            )));
        }
        if !arms.insert(item.arm) {
            return Err(error(format!("duplicate arm for task {}", item.task_id)));
        }
    }
    if by_task
        .values()
        .any(|(_, arms)| arms.len() != Arm::ALL.len())
    {
        return Err(error(
            "every task needs raw, lossless, lossy, and ccr observations",
        ));
    }

    let mut arms = Vec::with_capacity(4);
    for arm in Arm::ALL {
        let rows: Vec<_> = input
            .observations
            .iter()
            .filter(|row| row.arm == arm)
            .collect();
        let tasks = rows.len();
        let successes = rows.iter().filter(|row| row.success).count();
        let total_cost_millicents = (!unbilled)
            .then(|| {
                checked_sum(
                    rows.iter()
                        .map(|row| row.billed_cost_millicents.expect("billing validated")),
                    "cost",
                )
            })
            .transpose()?;
        let output_tokens = checked_sum(
            rows.iter()
                .map(|row| row.telemetry.provider_usage.output_tokens),
            "output tokens",
        )?;
        let cache_read_tokens = checked_sum(
            rows.iter()
                .map(|row| row.telemetry.provider_usage.cache_read_tokens),
            "cache read tokens",
        )?;
        let input_tokens = checked_sum(
            rows.iter()
                .map(|row| row.telemetry.provider_usage.input_tokens),
            "input tokens",
        )?;
        let cache_write_tokens = checked_sum(
            rows.iter()
                .map(|row| row.telemetry.provider_usage.cache_write_tokens),
            "cache write tokens",
        )?;
        let cache_input_tokens = input_tokens
            .checked_add(cache_read_tokens)
            .and_then(|n| n.checked_add(cache_write_tokens))
            .ok_or_else(|| error("cache input token overflow"))?;
        let retrieval_attempts = checked_sum(
            rows.iter().map(|row| row.telemetry.ccr_retrieve_attempts),
            "retrieval attempts",
        )?;
        let retrieval_misses = checked_sum(
            rows.iter().map(|row| row.telemetry.ccr_retrieve_misses),
            "retrieval misses",
        )?;
        let usage_complete_tasks = rows
            .iter()
            .filter(|row| row.telemetry.provider_rounds == row.telemetry.usage_reported_rounds)
            .count();
        let mut latencies: Vec<_> = rows
            .iter()
            .map(|row| row.telemetry.elapsed_millis)
            .collect();
        arms.push(ArmReport {
            arm,
            tasks,
            successes,
            success_rate: successes as f64 / tasks as f64,
            total_cost_millicents,
            cost_per_success_millicents: total_cost_millicents
                .filter(|_| successes > 0)
                .map(|total| total as f64 / successes as f64),
            p95_latency_millis: p95(&mut latencies),
            output_tokens,
            cache_read_tokens,
            cache_input_ratio: ratio(cache_read_tokens, cache_input_tokens),
            usage_complete_tasks,
            retrieval_attempts,
            retrieval_misses,
            retrieval_miss_rate: ratio(retrieval_misses, retrieval_attempts),
        });
    }
    let raw = &arms[0];
    let ccr = &arms[3];
    Ok(Report {
        version: "ccr-paired-task-v2",
        cost_source: input.cost_source,
        paired_tasks: by_task.len(),
        ccr_success_delta_vs_raw: ccr.success_rate - raw.success_rate,
        ccr_cost_per_success_delta_vs_raw_millicents: ccr
            .cost_per_success_millicents
            .zip(raw.cost_per_success_millicents)
            .map(|(ccr, raw)| ccr - raw),
        arms,
        limitations: vec![
            if unbilled {
                "No independently checked billing receipt was supplied; cost and cost-per-success are unavailable."
            } else {
                "Cost values and task-success labels are supplied by the caller and are not independently attested."
            },
            "Synthetic observations validate the evaluator only; they do not show live task quality or savings.",
            "A complete task comparison also needs identical prompts, fixtures, model settings, and source permissions across arms.",
        ],
    })
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayOrigin {
    Synthetic,
    RecordedClaim,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayEvidence {
    pub version: String,
    pub origin: ReplayOrigin,
    pub tasks: Vec<ReplayTask>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayTask {
    pub task_id: String,
    pub oracle_exact: String,
    pub arms: Vec<ReplayArm>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayArm {
    pub arm: Arm,
    pub task_input: String,
    pub source_result: String,
    /// Hash of the source tool result before the native CCR middleware runs.
    /// Optional for older recordings; a hash is a caller claim, not attestation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_source_sha256: Option<String>,
    /// Exact bytes of the recorded model settings JSON. Whitespace changes
    /// intentionally produce a different condition digest.
    pub model_settings_json: String,
    pub authorization: ReplayAuthorization,
    pub responses: Vec<ChatResponse>,
    pub telemetry: ToolLoopTelemetry,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReplayAuthorization {
    pub tenant_id: String,
    pub agent_id: String,
    pub session_id: String,
    pub principal_id: String,
    pub source_acl: String,
    pub source_server: String,
    pub source_tool: String,
    pub provider_id: String,
    pub model_id: String,
}

impl ReplayAuthorization {
    fn valid(&self) -> bool {
        let fields = [
            &self.tenant_id,
            &self.agent_id,
            &self.session_id,
            &self.principal_id,
            &self.source_acl,
            &self.source_server,
            &self.source_tool,
            &self.provider_id,
            &self.model_id,
        ];
        fields.iter().all(|field| {
            !field.trim().is_empty() && field.len() <= 512 && !field.chars().any(char::is_control)
        }) && crate::ccr_runtime::source_acl_for_principal(
            &self.agent_id,
            &self.session_id,
            &self.principal_id,
        )
        .as_deref()
            == Some(self.source_acl.as_str())
            && self.source_tool != CCR_FIND_TOOL
            && self.source_tool != CCR_RETRIEVE_TOOL
    }
}

fn digest_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn digest_fields(fields: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"ccr-replay-v1\0");
    for field in fields {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    hex::encode(hash.finalize())
}

fn checked_usage_sum(responses: &[ChatResponse]) -> Result<(NormalizedUsage, u64)> {
    let mut sum = NormalizedUsage::default();
    let mut reported = 0_u64;
    for response in responses {
        let usage = response.usage;
        if [
            usage.input_tokens,
            usage.output_tokens,
            usage.cache_read_tokens,
            usage.cache_write_tokens,
            usage.reasoning_tokens,
        ]
        .iter()
        .any(|value| *value > 0)
        {
            reported += 1;
        }
        sum.input_tokens = sum
            .input_tokens
            .checked_add(usage.input_tokens)
            .ok_or_else(|| error("replay input token overflow"))?;
        sum.output_tokens = sum
            .output_tokens
            .checked_add(usage.output_tokens)
            .ok_or_else(|| error("replay output token overflow"))?;
        sum.cache_read_tokens = sum
            .cache_read_tokens
            .checked_add(usage.cache_read_tokens)
            .ok_or_else(|| error("replay cache read token overflow"))?;
        sum.cache_write_tokens = sum
            .cache_write_tokens
            .checked_add(usage.cache_write_tokens)
            .ok_or_else(|| error("replay cache write token overflow"))?;
        sum.reasoning_tokens = sum
            .reasoning_tokens
            .checked_add(usage.reasoning_tokens)
            .ok_or_else(|| error("replay reasoning token overflow"))?;
    }
    Ok((sum, reported))
}

/// Deterministically replay an exact-value check from locally supplied
/// evidence. This checks consistency of the recorded rounds and conditions;
/// it cannot attest a provider identity, upstream ACL, or invoice.
pub fn replay_observations(evidence: ReplayEvidence) -> Result<(ReplayOrigin, Input)> {
    if evidence.version != "ccr-native-replay-v1"
        || evidence.tasks.is_empty()
        || evidence.tasks.len() > 1_000
    {
        return Err(error("expected 1 to 1000 ccr-native-replay-v1 tasks"));
    }
    let mut observations = Vec::new();
    for task in evidence.tasks {
        if task.task_id.is_empty()
            || task.task_id.len() > 128
            || task.task_id.chars().any(char::is_control)
        {
            return Err(error("invalid replay task ID"));
        }
        // Never echo a caller-supplied task label in reports or errors; it may
        // itself contain ticket or customer text.
        let task_id = digest_bytes(task.task_id.as_bytes());
        if !(3..=1_024).contains(&task.oracle_exact.len())
            || task.oracle_exact.chars().any(char::is_control)
            || task.arms.len() != 4
        {
            return Err(error(format!(
                "invalid replay oracle or arm count for {}",
                task_id
            )));
        }
        for arm in task.arms {
            let auth = &arm.authorization;
            let source_sha256 = digest_bytes(arm.source_result.as_bytes());
            if arm.delivered_source_sha256.as_ref().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || matches!(arm.arm, Arm::Raw | Arm::Ccr) && digest != &source_sha256
            }) {
                return Err(error("invalid source delivery digest"));
            }
            if !auth.valid()
                || arm.task_input.is_empty()
                || arm.task_input.len() > 65_536
                || arm.source_result.is_empty()
                || arm.source_result.len() > 2 * 1024 * 1024
                || !arm.source_result.contains(&task.oracle_exact)
                || arm.model_settings_json.len() > 8_192
                || !matches!(
                    serde_json::from_str::<Value>(&arm.model_settings_json),
                    Ok(Value::Object(_))
                )
                || arm.responses.is_empty()
                || arm.responses.len() > 64
            {
                return Err(error(format!(
                    "invalid replay material for {} {:?}",
                    task_id, arm.arm
                )));
            }
            let mut saw_source_call = false;
            let mut find_calls = 0_u64;
            let mut retrieve_calls = 0_u64;
            for response in &arm.responses {
                if response.provider != auth.provider_id || response.model_used != auth.model_id {
                    return Err(error(format!(
                        "provider/model claim changed for {} {:?}",
                        task_id, arm.arm
                    )));
                }
                for call in response.tool_calls() {
                    let tool = call.1;
                    if tool == auth.source_tool {
                        saw_source_call = true;
                    } else if arm.arm == Arm::Ccr && tool == CCR_FIND_TOOL {
                        find_calls = find_calls
                            .checked_add(1)
                            .ok_or_else(|| error("find count overflow"))?;
                    } else if arm.arm == Arm::Ccr && tool == CCR_RETRIEVE_TOOL {
                        retrieve_calls = retrieve_calls
                            .checked_add(1)
                            .ok_or_else(|| error("retrieve count overflow"))?;
                    } else {
                        return Err(error(format!(
                            "unlisted replay tool for {} {:?}",
                            task_id, arm.arm
                        )));
                    }
                }
            }
            if !saw_source_call {
                return Err(error(format!(
                    "source route was not called for {} {:?}",
                    task_id, arm.arm
                )));
            }
            if arm.telemetry.ccr_find_attempts != find_calls
                || arm.telemetry.ccr_retrieve_attempts != retrieve_calls
            {
                return Err(error(format!(
                    "CCR tool-call count mismatch for {} {:?}",
                    task_id, arm.arm
                )));
            }
            let (usage, reported_rounds) = checked_usage_sum(&arm.responses)?;
            if arm.telemetry.provider_rounds != arm.responses.len() as u64
                || arm.telemetry.usage_reported_rounds != reported_rounds
                || arm.telemetry.provider_usage != usage
            {
                return Err(error(format!(
                    "replay rounds/usage mismatch for {} {:?}",
                    task_id, arm.arm
                )));
            }
            let final_response = arm.responses.last().expect("responses checked nonempty");
            let success = final_response.stop == StopReason::EndTurn
                && final_response.text().contains(&task.oracle_exact);
            let conditions = EvaluationConditions {
                task_input_sha256: digest_bytes(arm.task_input.as_bytes()),
                source_set_sha256: digest_bytes(arm.source_result.as_bytes()),
                model_settings_sha256: digest_fields(&[
                    &auth.provider_id,
                    &auth.model_id,
                    &arm.model_settings_json,
                ]),
                authorization_sha256: digest_fields(&[
                    &auth.tenant_id,
                    &auth.agent_id,
                    &auth.session_id,
                    &auth.principal_id,
                    &auth.source_acl,
                    &auth.source_server,
                    &auth.source_tool,
                ]),
            };
            observations.push(Observation {
                task_id: task_id.clone(),
                arm: arm.arm,
                conditions,
                success,
                billed_cost_millicents: None,
                telemetry: arm.telemetry,
            });
        }
    }
    Ok((
        evidence.origin,
        Input {
            cost_source: "unbilled_replay".into(),
            observations,
        },
    ))
}

pub fn replay_report(origin: ReplayOrigin, input: Input) -> Result<Value> {
    let observations = serde_json::to_value(&input.observations).map_err(error)?;
    let report = summarize(input)?;
    Ok(json!({
        "evidence_origin": origin,
        "quality_method": "recorded_exact_value_check",
        "billing_status": "unavailable_without_verified_receipts",
        "observations": observations,
        "report": report,
        "limitations": [
            "This replays exact-value scoring and condition checks from local user-supplied evidence; it does not rerun a model.",
            "ChatResponse rounds, provider/model identity, upstream authorization, and elapsed time are not independently attested.",
            "Dashboard CCR metrics are tenant aggregates without task labels and are not substituted for paired observations."
        ]
    }))
}

/// Evaluate one bounded raw JSON upload. `selected_tenant` binds every arm to
/// the admin-selected dashboard tenant; the CLI passes `None` for offline use.
/// Parse errors are deliberately generic because serde may include a raw,
/// caller-supplied JSON field name in its diagnostic.
pub fn evaluate_replay_bytes(bytes: &[u8], selected_tenant: Option<&str>) -> Result<Value> {
    if bytes.len() > MAX_REPLAY_INPUT_BYTES {
        return Err(error("replay evidence exceeds 16 MB"));
    }
    let evidence: ReplayEvidence =
        serde_json::from_slice(bytes).map_err(|_| error("invalid replay evidence JSON"))?;
    if let Some(tenant) = selected_tenant {
        if tenant.is_empty()
            || tenant.len() > 128
            || tenant.trim() != tenant
            || tenant.chars().any(char::is_control)
        {
            return Err(error("invalid tenant selector"));
        }
        if evidence.tasks.iter().any(|task| {
            task.arms
                .iter()
                .any(|arm| arm.authorization.tenant_id != tenant)
        }) {
            return Err(error("replay tenant scope mismatch"));
        }
    }
    // A complete set of delivery hashes lets the report disclose treatment
    // bypasses. For CCR, the native middleware counter describes the actual
    // transform after the source tool returned its unchanged result.
    let delivery = evidence
        .tasks
        .iter()
        .all(|task| {
            task.arms
                .iter()
                .all(|arm| arm.delivered_source_sha256.is_some())
        })
        .then(|| {
            Arm::ALL
                .into_iter()
                .map(|name| {
                    let changed_tasks = evidence
                        .tasks
                        .iter()
                        .flat_map(|task| &task.arms)
                        .filter(|arm| arm.arm == name)
                        .filter(|arm| {
                            if name == Arm::Ccr {
                                arm.telemetry.ccr_compressed_results > 0
                            } else {
                                arm.delivered_source_sha256.as_deref()
                                    != Some(digest_bytes(arm.source_result.as_bytes()).as_str())
                            }
                        })
                        .count();
                    json!({"arm": name, "changed_tasks": changed_tasks})
                })
                .collect::<Vec<_>>()
        });
    let (origin, input) = replay_observations(evidence)?;
    let mut report = replay_report(origin, input)?;
    report["source_delivery"] = json!(delivery);
    Ok(report)
}
