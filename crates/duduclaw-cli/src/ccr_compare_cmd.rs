//! Paired task-level CCR evaluation from externally supplied observations.

use std::{fs, path::Path, sync::Mutex};

use async_trait::async_trait;
use duduclaw_core::error::Result;
use duduclaw_gateway::ccr_replay::{
    Arm, EvaluationConditions, Input, MAX_REPLAY_INPUT_BYTES, Observation, error,
    evaluate_replay_bytes, summarize,
};
use duduclaw_llm::{
    CCR_FIND_TOOL, CCR_RETRIEVE_TOOL, CcrRuntime, CcrScope, CcrStore, ChatMessage, ChatProvider,
    ChatRequest, ChatResponse, ContentPart, LlmError, NormalizedUsage, ProvenanceConfig, Role,
    StopReason, StreamEvent, ToolDef, ToolExecutor, ToolOutcome,
    run_tool_loop_with_provenance_and_ccr,
};
use futures_util::stream::BoxStream;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const MAX_INPUT_BYTES: u64 = 2_000_000;

pub fn evaluate(path: &Path) -> Result<()> {
    let size = fs::metadata(path).map_err(error)?.len();
    if size > MAX_INPUT_BYTES {
        return Err(error("observation file exceeds 2 MB"));
    }
    let input: Input = serde_json::from_slice(&fs::read(path).map_err(error)?).map_err(error)?;
    if input.cost_source == "unbilled_replay" {
        return Err(error("use ccr-compare-replay for unbilled evidence"));
    }
    let report = summarize(input)?;
    println!("{}", serde_json::to_string_pretty(&report).map_err(error)?);
    Ok(())
}

pub fn evaluate_replay(path: &Path) -> Result<()> {
    if fs::metadata(path).map_err(error)?.len() > MAX_REPLAY_INPUT_BYTES as u64 {
        return Err(error("replay evidence file exceeds 16 MB"));
    }
    let output = evaluate_replay_bytes(&fs::read(path).map_err(error)?, None)?;
    println!("{}", serde_json::to_string_pretty(&output).map_err(error)?);
    Ok(())
}

struct SyntheticProvider {
    required: String,
    lookup: String,
    rounds: Mutex<u64>,
}

impl SyntheticProvider {
    fn response(&self, req: &ChatRequest) -> ChatResponse {
        let mut rounds = self.rounds.lock().expect("synthetic round lock");
        let round = *rounds;
        *rounds += 1;
        let last_tool = req.messages.iter().rev().find_map(|message| {
            (message.role == Role::Assistant)
                .then(|| {
                    message.parts.iter().find_map(|part| match part {
                        ContentPart::ToolCall { name, .. } => Some(name.as_str()),
                        _ => None,
                    })
                })
                .flatten()
        });
        let last_result = req
            .messages
            .last()
            .and_then(|message| {
                message.parts.iter().find_map(|part| match part {
                    ContentPart::ToolResult { content, .. } => Some(content.as_str()),
                    _ => None,
                })
            })
            .unwrap_or_default();
        let (parts, stop) = if round == 0 {
            (
                vec![ContentPart::ToolCall {
                    id: "source-call".into(),
                    name: "fixture_read".into(),
                    args: json!({}),
                }],
                StopReason::ToolUse,
            )
        } else if last_tool == Some(CCR_FIND_TOOL) {
            let hit = serde_json::from_str::<Value>(last_result)
                .ok()
                .and_then(|value| {
                    let hit = value.get("hits")?.as_array()?.first()?;
                    Some((
                        hit.get("id")?.as_str()?.to_owned(),
                        hit.get("exact_phrase")?.as_bool()?,
                        hit.get("byte_offset")?.as_u64()?,
                    ))
                });
            match hit {
                Some((id, exact_phrase, byte_offset)) => (
                    vec![ContentPart::ToolCall {
                        id: "retrieve-call".into(),
                        name: CCR_RETRIEVE_TOOL.into(),
                        args: if exact_phrase {
                            json!({"id": id, "query": self.lookup})
                        } else {
                            json!({"id": id, "offset": byte_offset, "limit": 512})
                        },
                    }],
                    StopReason::ToolUse,
                ),
                None => (
                    vec![ContentPart::Text("MISSING".into())],
                    StopReason::EndTurn,
                ),
            }
        } else if last_result.contains(&self.required) {
            (vec![ContentPart::Text("FOUND".into())], StopReason::EndTurn)
        } else if last_tool == Some("fixture_read")
            && req.tools.iter().any(|tool| tool.name == CCR_FIND_TOOL)
        {
            (
                vec![ContentPart::ToolCall {
                    id: "find-call".into(),
                    name: CCR_FIND_TOOL.into(),
                    args: json!({"query": self.lookup}),
                }],
                StopReason::ToolUse,
            )
        } else {
            (
                vec![ContentPart::Text("MISSING".into())],
                StopReason::EndTurn,
            )
        };
        let request_text = serde_json::to_string(req).expect("serializable synthetic request");
        let output_text = serde_json::to_string(&parts).expect("serializable synthetic response");
        ChatResponse {
            parts,
            stop,
            usage: NormalizedUsage {
                input_tokens: duduclaw_llm::estimate_tokens(&request_text),
                output_tokens: duduclaw_llm::estimate_tokens(&output_text),
                ..Default::default()
            },
            model_used: "synthetic-mock-v1".into(),
            provider: "synthetic".into(),
        }
    }
}

#[async_trait]
impl ChatProvider for SyntheticProvider {
    fn id(&self) -> &str {
        "synthetic"
    }

    async fn complete(&self, req: &ChatRequest) -> std::result::Result<ChatResponse, LlmError> {
        Ok(self.response(req))
    }

    async fn stream(
        &self,
        _req: &ChatRequest,
    ) -> std::result::Result<BoxStream<'static, std::result::Result<StreamEvent, LlmError>>, LlmError>
    {
        Err(LlmError::InvalidRequest(
            "synthetic stream is unsupported".into(),
        ))
    }
}

struct SyntheticExecutor {
    content: String,
}

#[async_trait]
impl ToolExecutor for SyntheticExecutor {
    fn defs(&self) -> Vec<ToolDef> {
        vec![ToolDef {
            name: "fixture_read".into(),
            description: "Read the current synthetic source".into(),
            input_schema: json!({"type":"object","properties":{}}),
        }]
    }

    fn server_of(&self, tool: &str) -> Option<String> {
        (tool == "fixture_read").then(|| "synthetic-eval".into())
    }

    async fn call(&self, name: &str, _args: Value) -> std::result::Result<ToolOutcome, String> {
        if name == "fixture_read" {
            Ok(ToolOutcome::ok(self.content.clone()))
        } else {
            Err(format!("unexpected synthetic tool: {name}"))
        }
    }
}

fn digest(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

pub(super) fn arm_delivery(
    arm: Arm,
    original: &str,
    query: Option<&str>,
    runtime: &CcrRuntime,
) -> String {
    match arm {
        Arm::Raw | Arm::Ccr => original.to_owned(),
        Arm::Lossless => CcrRuntime::lossless_compact_structured(original)
            .filter(|compact| compact.len() < original.len())
            .unwrap_or_else(|| original.to_owned()),
        Arm::Lossy => runtime
            .preview_with_query(original, "unavailable", query)
            .map(|preview| {
                let marker = format!(
                    "[CCR: {} bytes; retrieve with {CCR_RETRIEVE_TOOL} id=unavailable]",
                    original.len()
                );
                preview
                    .strip_suffix(&format!("\n{marker}"))
                    .map(str::to_owned)
                    .unwrap_or_else(|| {
                        preview.replace(&marker, "[preview: omitted content unavailable]")
                    })
            })
            .unwrap_or_else(|| original.to_owned()),
    }
}

fn synthetic_lookup(
    task_id: &str,
    required: &str,
    query: Option<&str>,
    find_query: Option<&str>,
) -> String {
    if let Some(find_query) = find_query {
        return find_query.into();
    }
    match task_id {
        "buried_exact_value" => "UNIQUE-MIDDLE".into(),
        "search_middle_hit" => "case-791".into(),
        _ => query
            .unwrap_or_else(|| required.split_whitespace().next().unwrap_or(required))
            .into(),
    }
}

async fn synthetic_observations() -> Result<Input> {
    let mut observations = Vec::new();
    for (task_id, original, required, query, find_query) in
        super::ccr_eval_cmd::synthetic_task_fixtures()
    {
        let lookup = synthetic_lookup(task_id, required, query, find_query);
        let task_prompt = format!("Find the exact source value associated with {lookup}");
        let conditions = EvaluationConditions {
            task_input_sha256: digest(&task_prompt),
            source_set_sha256: digest(&original),
            model_settings_sha256: digest("synthetic-mock-v1;temperature=0;tools=fixture_read"),
            authorization_sha256: digest("tenant=synthetic;agent=ccr-eval;acl=private"),
        };
        for arm in Arm::ALL {
            let dir = tempfile::tempdir().map_err(error)?;
            let scope = CcrScope {
                tenant_id: "synthetic".into(),
                agent_id: "ccr-eval".into(),
                session_id: format!("{task_id}-{arm:?}"),
                source_acl: "private".into(),
            };
            let runtime = CcrRuntime::new(CcrStore::new(dir.path().join("ccr.db")), scope)
                .restrict_sources([("synthetic-eval".into(), "fixture_read".into())]);
            let executor = SyntheticExecutor {
                content: arm_delivery(arm, &original, query, &runtime),
            };
            let provider = SyntheticProvider {
                required: required.into(),
                lookup: lookup.clone(),
                rounds: Mutex::new(0),
            };
            let mut request = ChatRequest::new("synthetic-mock-v1");
            request
                .messages
                .push(ChatMessage::user(task_prompt.clone()));
            let outcome = run_tool_loop_with_provenance_and_ccr(
                &provider,
                request,
                &executor,
                4,
                ProvenanceConfig::default(),
                None,
                (arm == Arm::Ccr).then_some(runtime),
            )
            .await
            .map_err(error)?;
            if outcome.response.stop != StopReason::EndTurn {
                return Err(error(format!(
                    "synthetic loop did not finish: {task_id} {arm:?}"
                )));
            }
            let success = outcome
                .response
                .parts
                .iter()
                .any(|part| matches!(part, ContentPart::Text(text) if text == "FOUND"));
            let usage = outcome.telemetry.provider_usage;
            let synthetic_cost = usage
                .output_tokens
                .checked_mul(5)
                .and_then(|output_cost| usage.input_tokens.checked_add(output_cost))
                .ok_or_else(|| error("synthetic cost overflow"))?;
            observations.push(Observation {
                task_id: task_id.into(),
                arm,
                conditions: conditions.clone(),
                success,
                billed_cost_millicents: Some(synthetic_cost),
                telemetry: outcome.telemetry,
            });
        }
    }
    Ok(Input {
        cost_source: "synthetic".into(),
        observations,
    })
}

pub async fn evaluate_synthetic() -> Result<()> {
    let input = synthetic_observations().await?;
    let observations = serde_json::to_value(&input).map_err(error)?;
    let report = summarize(input)?;
    println!("{}", serde_json::to_string_pretty(&json!({
        "cost_model": "synthetic mock usage; 1 millicent per estimated input token and 5 per estimated output token",
        "input": observations,
        "report": report,
    })).map_err(error)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    // Only the test fixtures build replay evidence by hand; importing these in
    // the module header made them unused in a non-test build.
    use duduclaw_gateway::ccr_replay::{
        ReplayArm, ReplayAuthorization, ReplayEvidence, ReplayOrigin, ReplayTask,
        replay_observations, replay_report,
    };
    use duduclaw_llm::ToolLoopTelemetry;

    fn replay_fixture() -> ReplayEvidence {
        let source_acl =
            duduclaw_gateway::ccr_runtime::source_acl_for_principal("agent", "session", "user-1")
                .unwrap();
        let arms = Arm::ALL
            .into_iter()
            .map(|arm| ReplayArm {
                arm,
                task_input: "Find the exact ticket value".into(),
                source_result: "Ticket evidence EXACT-001".into(),
                delivered_source_sha256: None,
                model_settings_json: "{\"temperature\":0,\"tools\":[\"fixture_read\"]}".into(),
                authorization: ReplayAuthorization {
                    tenant_id: "local".into(),
                    agent_id: "agent".into(),
                    session_id: "session".into(),
                    principal_id: "user-1".into(),
                    source_acl: source_acl.clone(),
                    source_server: "synthetic-eval".into(),
                    source_tool: "fixture_read".into(),
                    provider_id: "synthetic".into(),
                    model_id: "mock-v1".into(),
                },
                responses: vec![
                    ChatResponse {
                        parts: vec![ContentPart::ToolCall {
                            id: "source-call".into(),
                            name: "fixture_read".into(),
                            args: json!({}),
                        }],
                        stop: StopReason::ToolUse,
                        usage: NormalizedUsage {
                            input_tokens: 10,
                            output_tokens: 2,
                            ..Default::default()
                        },
                        model_used: "mock-v1".into(),
                        provider: "synthetic".into(),
                    },
                    ChatResponse {
                        parts: vec![ContentPart::Text("The value is EXACT-001".into())],
                        stop: StopReason::EndTurn,
                        usage: NormalizedUsage {
                            input_tokens: 20,
                            output_tokens: 3,
                            ..Default::default()
                        },
                        model_used: "mock-v1".into(),
                        provider: "synthetic".into(),
                    },
                ],
                telemetry: ToolLoopTelemetry {
                    provider_rounds: 2,
                    usage_reported_rounds: 2,
                    provider_usage: NormalizedUsage {
                        input_tokens: 30,
                        output_tokens: 5,
                        ..Default::default()
                    },
                    elapsed_millis: 50,
                    ..Default::default()
                },
            })
            .collect();
        ReplayEvidence {
            version: "ccr-native-replay-v1".into(),
            origin: ReplayOrigin::Synthetic,
            tasks: vec![ReplayTask {
                task_id: "ticket-exact".into(),
                oracle_exact: "EXACT-001".into(),
                arms,
            }],
        }
    }

    #[test]
    fn replay_derives_conditions_quality_and_unbilled_cost() {
        let mut evidence = replay_fixture();
        evidence.tasks[0].task_id = "Private customer ticket subject".into();
        let encoded = serde_json::to_vec(&evidence).unwrap();
        let parsed: ReplayEvidence = serde_json::from_slice(&encoded).unwrap();
        let (origin, input) = replay_observations(parsed).unwrap();
        assert_eq!(input.cost_source, "unbilled_replay");
        assert_eq!(input.observations.len(), 4);
        assert!(input.observations.iter().all(|row| row.success));
        let output = replay_report(origin, input.clone()).unwrap();
        let rendered = output.to_string();
        assert!(!rendered.contains("Ticket evidence EXACT-001"));
        assert!(!rendered.contains("Find the exact ticket value"));
        assert!(!rendered.contains("The value is EXACT-001"));
        assert!(!rendered.contains("user-1"));
        assert!(!rendered.contains("Private customer ticket subject"));
        assert_eq!(
            output["billing_status"],
            "unavailable_without_verified_receipts"
        );
        let report = summarize(input).unwrap();
        assert_eq!(report.paired_tasks, 1);
        assert!(
            report
                .arms
                .iter()
                .all(|arm| arm.total_cost_millicents.is_none()
                    && arm.cost_per_success_millicents.is_none())
        );
        assert!(
            report
                .ccr_cost_per_success_delta_vs_raw_millicents
                .is_none()
        );
    }

    #[test]
    fn replay_ccr_counters_must_match_recorded_find_and_retrieve_calls() {
        let mut evidence = replay_fixture();
        evidence.tasks[0].arms[3].telemetry.ccr_find_attempts = 1;
        evidence.tasks[0].arms[3].telemetry.ccr_find_misses = 1;
        assert!(replay_observations(evidence.clone()).is_err());

        evidence.tasks[0].arms[3].responses[0]
            .parts
            .push(ContentPart::ToolCall {
                id: "find-call".into(),
                name: CCR_FIND_TOOL.into(),
                args: json!({"query":"EXACT-001"}),
            });
        assert!(replay_observations(evidence.clone()).is_ok());

        evidence.tasks[0].arms[3].telemetry.ccr_retrieve_attempts = 1;
        evidence.tasks[0].arms[3].telemetry.ccr_retrieve_misses = 1;
        assert!(replay_observations(evidence.clone()).is_err());
        evidence.tasks[0].arms[3].responses[0]
            .parts
            .push(ContentPart::ToolCall {
                id: "retrieve-call".into(),
                name: CCR_RETRIEVE_TOOL.into(),
                args: json!({"id":"opaque-handle"}),
            });
        assert!(replay_observations(evidence).is_ok());
    }

    #[test]
    fn replay_rejects_changed_source_scope_route_and_usage() {
        let mut changed = replay_fixture();
        changed.tasks[0].arms[1].source_result = "Ticket evidence EXACT-001 changed".into();
        let (_, input) = replay_observations(changed).unwrap();
        assert!(summarize(input).is_err());

        let mut changed_task = replay_fixture();
        changed_task.tasks[0].arms[1].task_input = "Find a different ticket value".into();
        let (_, input) = replay_observations(changed_task).unwrap();
        assert!(summarize(input).is_err());

        let mut changed_settings = replay_fixture();
        changed_settings.tasks[0].arms[1].model_settings_json = "{\"temperature\":1}".into();
        let (_, input) = replay_observations(changed_settings).unwrap();
        assert!(summarize(input).is_err());

        let mut changed_server = replay_fixture();
        changed_server.tasks[0].arms[1].authorization.source_server = "other-server".into();
        let (_, input) = replay_observations(changed_server).unwrap();
        assert!(summarize(input).is_err());

        let mut wrong_scope = replay_fixture();
        wrong_scope.tasks[0].arms[1].authorization.principal_id = "user-2".into();
        assert!(replay_observations(wrong_scope).is_err());

        let mut wrong_route = replay_fixture();
        wrong_route.tasks[0].arms[1].authorization.source_tool = "other_read".into();
        assert!(replay_observations(wrong_route).is_err());

        let mut wrong_model_claim = replay_fixture();
        wrong_model_claim.tasks[0].arms[1].responses[0].model_used = "other-model".into();
        assert!(replay_observations(wrong_model_claim).is_err());

        let mut wrong_usage = replay_fixture();
        wrong_usage.tasks[0].arms[1]
            .telemetry
            .provider_usage
            .input_tokens += 1;
        assert!(replay_observations(wrong_usage).is_err());
    }

    #[test]
    fn replay_marks_missing_exact_value_as_failure_without_cost() {
        let mut evidence = replay_fixture();
        evidence.tasks[0].arms[3].responses[1].parts =
            vec![ContentPart::Text("The value is unavailable".into())];
        let (_, input) = replay_observations(evidence).unwrap();
        assert!(!input.observations[3].success);
        let report = summarize(input).unwrap();
        assert_eq!(report.arms[3].successes, 0);
        assert!(report.arms[3].cost_per_success_millicents.is_none());
    }

    #[test]
    fn replay_fixture_file_recomputes_four_arm_quality_without_billing() {
        let evidence: ReplayEvidence = serde_json::from_str(include_str!(
            "../../../fixtures/ccr/native-replay-synthetic.json"
        ))
        .unwrap();
        let (origin, input) = replay_observations(evidence).unwrap();
        let output = replay_report(origin, input).unwrap();
        assert_eq!(output["quality_method"], "recorded_exact_value_check");
        assert_eq!(output["report"]["paired_tasks"], 1);
        assert_eq!(output["report"]["arms"][2]["successes"], 0);
        assert!(
            output["report"]["arms"]
                .as_array()
                .unwrap()
                .iter()
                .all(|arm| arm["total_cost_millicents"].is_null())
        );
    }

    fn row(task_id: &str, arm: Arm, success: bool, cost: u64, latency: u128) -> Observation {
        Observation {
            task_id: task_id.into(),
            arm,
            conditions: EvaluationConditions {
                task_input_sha256: "a".repeat(64),
                source_set_sha256: "b".repeat(64),
                model_settings_sha256: "c".repeat(64),
                authorization_sha256: "d".repeat(64),
            },
            success,
            billed_cost_millicents: Some(cost),
            telemetry: ToolLoopTelemetry {
                provider_rounds: 1,
                usage_reported_rounds: 1,
                elapsed_millis: latency,
                ..Default::default()
            },
        }
    }

    #[test]
    fn paired_cost_counts_failed_attempts_in_cost_per_success() {
        let mut observations = Vec::new();
        for task in ["a", "b"] {
            for arm in Arm::ALL {
                observations.push(row(
                    task,
                    arm,
                    task == "a" || arm == Arm::Raw,
                    if arm == Arm::Ccr { 30 } else { 50 },
                    if arm == Arm::Ccr { 90 } else { 100 },
                ));
            }
        }
        let report = summarize(Input {
            cost_source: "synthetic".into(),
            observations,
        })
        .unwrap();
        assert_eq!(report.paired_tasks, 2);
        assert_eq!(report.arms[0].cost_per_success_millicents, Some(50.0));
        assert_eq!(report.arms[3].cost_per_success_millicents, Some(60.0));
        assert_eq!(
            report.ccr_cost_per_success_delta_vs_raw_millicents,
            Some(10.0)
        );
        assert_eq!(report.ccr_success_delta_vs_raw, -0.5);
    }

    #[test]
    fn partial_or_unlabeled_billing_is_rejected() {
        let mut observations: Vec<_> = Arm::ALL
            .into_iter()
            .map(|arm| row("a", arm, true, 1, 1))
            .collect();
        observations[0].billed_cost_millicents = None;
        assert!(
            summarize(Input {
                cost_source: "synthetic".into(),
                observations: observations.clone(),
            })
            .is_err()
        );
        assert!(
            summarize(Input {
                cost_source: "unbilled_replay".into(),
                observations,
            })
            .is_err()
        );
    }

    #[test]
    fn incomplete_or_duplicate_pair_is_rejected() {
        let rows = vec![row("a", Arm::Raw, true, 1, 1)];
        assert!(
            summarize(Input {
                cost_source: "synthetic".into(),
                observations: rows
            })
            .is_err()
        );
        let rows = Arm::ALL
            .into_iter()
            .map(|arm| row("a", arm, true, 1, 1))
            .chain(std::iter::once(row("a", Arm::Ccr, true, 1, 1)))
            .collect();
        assert!(
            summarize(Input {
                cost_source: "synthetic".into(),
                observations: rows
            })
            .is_err()
        );
    }

    #[test]
    fn changed_source_or_model_conditions_refuse_pair() {
        let mut rows: Vec<_> = Arm::ALL
            .into_iter()
            .map(|arm| row("a", arm, true, 1, 1))
            .collect();
        rows[3].conditions.source_set_sha256 = "e".repeat(64);
        assert!(
            summarize(Input {
                cost_source: "synthetic".into(),
                observations: rows
            })
            .is_err()
        );
    }

    #[tokio::test]
    async fn synthetic_loop_recovers_buried_value_through_real_ccr_find_and_retrieve() {
        let input = synthetic_observations().await.unwrap();
        assert_eq!(input.observations.len(), 48);
        let buried = input
            .observations
            .iter()
            .filter(|row| row.task_id == "buried_exact_value")
            .collect::<Vec<_>>();
        assert_eq!(buried.len(), 4);
        assert!(
            buried
                .iter()
                .find(|row| row.arm == Arm::Raw)
                .unwrap()
                .success
        );
        assert!(
            !buried
                .iter()
                .find(|row| row.arm == Arm::Lossy)
                .unwrap()
                .success
        );
        let ccr = buried.iter().find(|row| row.arm == Arm::Ccr).unwrap();
        assert!(ccr.success);
        assert_eq!(ccr.telemetry.ccr_find_hits, 1);
        assert_eq!(ccr.telemetry.ccr_retrieve_successes, 1);
        assert_eq!(ccr.telemetry.provider_rounds, 4);
        assert_eq!(ccr.telemetry.usage_reported_rounds, 4);
        let lexical = input
            .observations
            .iter()
            .filter(|row| row.task_id == "cross_turn_multi_term")
            .collect::<Vec<_>>();
        assert_eq!(lexical.len(), 4);
        assert!(
            !lexical
                .iter()
                .find(|row| row.arm == Arm::Lossy)
                .unwrap()
                .success
        );
        let lexical_ccr = lexical.iter().find(|row| row.arm == Arm::Ccr).unwrap();
        assert!(lexical_ccr.success);
        assert_eq!(lexical_ccr.telemetry.ccr_find_hits, 1);
        assert_eq!(lexical_ccr.telemetry.ccr_retrieve_successes, 1);
        let han = input
            .observations
            .iter()
            .filter(|row| row.task_id == "cross_turn_han")
            .collect::<Vec<_>>();
        assert_eq!(han.len(), 4);
        assert!(
            !han.iter()
                .find(|row| row.arm == Arm::Lossy)
                .unwrap()
                .success
        );
        let han_ccr = han.iter().find(|row| row.arm == Arm::Ccr).unwrap();
        assert!(han_ccr.success);
        assert_eq!(han_ccr.telemetry.ccr_find_hits, 1);
        assert_eq!(han_ccr.telemetry.ccr_retrieve_successes, 1);
        let middle = input
            .observations
            .iter()
            .filter(|row| row.task_id == "search_multi_term_middle")
            .collect::<Vec<_>>();
        assert_eq!(middle.len(), 4);
        assert!(middle.iter().all(|row| row.success));
        let han_middle = input
            .observations
            .iter()
            .filter(|row| row.task_id == "search_han_middle")
            .collect::<Vec<_>>();
        assert_eq!(han_middle.len(), 4);
        assert!(han_middle.iter().all(|row| row.success));
        let duplicate = input
            .observations
            .iter()
            .filter(|row| row.task_id == "duplicate_key_numeric_lexeme")
            .collect::<Vec<_>>();
        assert_eq!(duplicate.len(), 4);
        assert!(duplicate.iter().all(|row| row.success));
        assert_eq!(summarize(input).unwrap().paired_tasks, 12);
    }

    #[test]
    fn synthetic_lossless_arm_preserves_duplicate_keys_and_numeric_spelling() {
        let (_, original, required, _, _) = crate::ccr_eval_cmd::synthetic_task_fixtures()
            .into_iter()
            .find(|(name, ..)| *name == "duplicate_key_numeric_lexeme")
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let runtime = CcrRuntime::new(
            CcrStore::new(dir.path().join("ccr.db")),
            CcrScope {
                tenant_id: "synthetic".into(),
                agent_id: "ccr-eval".into(),
                session_id: "lossless".into(),
                source_acl: "private".into(),
            },
        );
        let compact = arm_delivery(Arm::Lossless, &original, None, &runtime);
        assert!(compact.len() < original.len());
        assert_eq!(compact.matches("\"priority\"").count(), 2);
        assert!(compact.contains(required));
        assert!(compact.contains("\"priority\":1.0e+02,\"priority\":100"));
    }
}
