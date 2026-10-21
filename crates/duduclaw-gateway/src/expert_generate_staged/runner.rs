//! One background generation job: model calls → materialize → swap in →
//! persist draft state, with progress, a configurable deadline, and rollback
//! so a failed, timed-out or cancelled round never loses the previous one.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use crate::expert_generate as eg;

use super::job::{JobRecord, JobStatus, ProgressSink, StepStatus};
use super::{Stage, StageLlm, StageReply, StagedRequest, run_staged};

/// Default whole-job deadline. A circuit breaker, not an expectation: a full
/// 8-member team at concurrency 2 is ~5 waves of model calls.
pub const DEFAULT_DEADLINE_MINUTES: u64 = 25;
const MIN_DEADLINE_MINUTES: u64 = 5;
const MAX_DEADLINE_MINUTES: u64 = 120;
/// Direct API per-request deadline for the long content stages.
const STAGE_HTTP_TIMEOUT_SECS: u64 = 300;

/// `<home>/config.toml` `[experts] generate_timeout_minutes` (5–120, default
/// 25). Unreadable or out-of-range values fall back to the default / clamp.
pub fn deadline_minutes(home: &Path) -> u64 {
    std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|t| t.parse::<toml::Table>().ok())
        .and_then(|t| {
            t.get("experts")
                .and_then(|e| e.get("generate_timeout_minutes"))
                .and_then(|v| v.as_integer())
        })
        .map(|m| (m.max(0) as u64).clamp(MIN_DEADLINE_MINUTES, MAX_DEADLINE_MINUTES))
        .unwrap_or(DEFAULT_DEADLINE_MINUTES)
}

/// Tool surface for one stage call: zero tools, except the knowledge-base
/// stage gets `WebSearch` when the operator opted in.
pub fn caps_for(stage: Stage, research: bool) -> duduclaw_core::types::CapabilitiesConfig {
    if research && stage == Stage::Wiki {
        crate::night_llm::research_capabilities()
    } else {
        crate::night_llm::night_capabilities()
    }
}

/// Rotated Claude CLI (subscription first) → Direct API fallback, per stage.
pub struct RotatedStageLlm {
    pub home_dir: PathBuf,
    pub research: bool,
}

#[async_trait::async_trait]
impl StageLlm for RotatedStageLlm {
    async fn complete(&self, stage: Stage, system: &str, user: &str) -> Result<StageReply, String> {
        let caps = caps_for(stage, self.research);
        let web_search = caps.allowed_tools().iter().any(|t| t == "WebSearch");
        let model = eg::GENERATE_MODEL;
        let cli_err = match crate::channel_reply::call_claude_cli_rotated(
            user,
            model,
            system,
            &self.home_dir,
            None,
            None,
            Some(&caps),
            None,
            &[],
            // System-level utility call, not an agent turn — no account pool.
            &[],
            None,
        )
        .await
        {
            Ok(text) if !text.trim().is_empty() => {
                return Ok(StageReply {
                    text,
                    web_search,
                    fallback: false,
                });
            }
            Ok(_) => "empty CLI response".to_string(),
            Err(e) => duduclaw_core::truncate_chars(&e, 200),
        };
        let api_key = crate::claude_runner::get_api_key_from_home(&self.home_dir).await;
        if api_key.is_empty() {
            return Err(format!("（{cli_err}），且未設定 API key 可作備援"));
        }
        let limits = crate::direct_api::RequestLimits {
            max_tokens: stage.max_tokens(),
            timeout: Some(std::time::Duration::from_secs(STAGE_HTTP_TIMEOUT_SECS)),
        };
        match crate::direct_api::call_direct_api_with_limits(&api_key, model, system, user, limits)
            .await
        {
            // The Direct API path has no tools: never a web-search reply.
            Ok(resp) if !resp.text.trim().is_empty() => Ok(StageReply {
                text: resp.text,
                web_search: false,
                fallback: true,
            }),
            Ok(_) => Err("模型回傳空內容".into()),
            Err(e) => Err(duduclaw_core::truncate_chars(&e, 200)),
        }
    }
}

/// Everything one background job needs (owned: it outlives the RPC).
pub struct JobSpec {
    pub home_dir: PathBuf,
    pub draft_id: String,
    pub kind: &'static str,
    pub req: eg::GenerateRequest,
    /// `Some` on revise: the state the new round builds on.
    pub prior_state: Option<eg::DraftState>,
    pub prior_pack: Option<eg::GeneratedPack>,
    pub feedback: Option<String>,
    pub deadline: std::time::Duration,
}

/// A new round swapped in, with the previous round kept until [`commit`].
///
/// [`commit`]: Swapped::commit
pub struct Swapped {
    live: PathBuf,
    prev: Option<PathBuf>,
}

impl Swapped {
    /// The new round is final: drop the previous one.
    pub fn commit(self) {
        if let Some(prev) = &self.prev
            && let Err(e) = std::fs::remove_dir_all(prev)
        {
            tracing::warn!(error = %e, "expert draft: could not remove the previous round");
        }
    }

    /// Put the previous round back (best effort; reports failure).
    pub fn rollback(self) -> Result<(), String> {
        let _ = std::fs::remove_dir_all(&self.live);
        match &self.prev {
            Some(prev) => std::fs::rename(prev, &self.live).map_err(|e| format!("還原上一輪草稿失敗: {e}")),
            None => Ok(()),
        }
    }
}

/// Materialize into `<draft>/pack-next`, validate, then swap it in. The old
/// `pack/` moves to `pack-prev/` and stays until [`Swapped::commit`].
pub fn materialize_and_swap(home: &Path, draft_id: &str, pack: &eg::GeneratedPack) -> Result<Swapped, String> {
    let draft_dir = eg::draft_dir(home, draft_id)?;
    let next = draft_dir.join("pack-next");
    eg::materialize_draft(&next, pack)?;
    let problems = eg::validate_draft_pack(&next);
    if !problems.is_empty() {
        let _ = std::fs::remove_dir_all(&next);
        return Err(format!(
            "草稿未通過驗證：{}",
            duduclaw_core::truncate_chars(&problems.join("；"), 600)
        ));
    }
    let live = eg::draft_pack_dir(home, draft_id)?;
    let prev = draft_dir.join("pack-prev");
    if prev.exists() {
        std::fs::remove_dir_all(&prev).map_err(|e| format!("替換草稿失敗: {e}"))?;
    }
    let had_live = live.exists();
    if had_live {
        std::fs::rename(&live, &prev).map_err(|e| format!("替換草稿失敗: {e}"))?;
    }
    if let Err(e) = std::fs::rename(&next, &live) {
        if had_live {
            let _ = std::fs::rename(&prev, &live);
        }
        return Err(format!("替換草稿失敗: {e}"));
    }
    Ok(Swapped {
        live,
        prev: had_live.then_some(prev),
    })
}

/// Result payload shared by the generate/revise/status responses.
pub fn draft_payload(home: &Path, state: &eg::DraftState, status: JobStatus) -> Value {
    json!({
        "draft_id": state.draft_id,
        "status": status,
        "rounds": state.rounds,
        "rounds_left": state.rounds_left(),
        "notes": state.notes,
        "preview": eg::draft_preview_json(home, state),
    })
}

fn next_state(spec: &JobSpec, pack: &eg::GeneratedPack, notes: &[String]) -> eg::DraftState {
    let now = crate::expert_admin::now_iso();
    let last_generation = serde_json::to_string(pack).unwrap_or_default();
    match spec.prior_state.clone() {
        Some(prev) => eg::DraftState {
            rounds: prev.rounds + 1,
            updated_at: now,
            last_generation,
            notes: notes.to_vec(),
            ..prev
        },
        None => eg::DraftState {
            draft_id: spec.draft_id.clone(),
            request: spec.req.clone(),
            rounds: 1,
            created_at: now.clone(),
            updated_at: now,
            last_generation,
            notes: notes.to_vec(),
        },
    }
}

/// Swap the new pack in and persist the state; if the state cannot be
/// written, the previous round is restored so pack and state never disagree.
fn assemble_round(spec: &JobSpec, pack: &eg::GeneratedPack, notes: &[String]) -> Result<eg::DraftState, String> {
    let swapped = materialize_and_swap(&spec.home_dir, &spec.draft_id, pack)?;
    let state = next_state(spec, pack, notes);
    match eg::write_draft_state(&spec.home_dir, &state) {
        Ok(()) => {
            swapped.commit();
            Ok(state)
        }
        Err(e) => {
            let restored = swapped.rollback();
            Err(match restored {
                Ok(()) => format!("{e}（已還原上一輪草稿）"),
                Err(r) => format!("{e}；{r}"),
            })
        }
    }
}

pub async fn run_job(spec: JobSpec, llm: &dyn StageLlm) -> Result<Value, String> {
    let draft_dir = eg::draft_dir(&spec.home_dir, &spec.draft_id)?;
    let sink = ProgressSink::new(draft_dir, JobRecord::new(spec.kind));
    sink.update(|_| {});
    let input = StagedRequest {
        req: &spec.req,
        installed_example: eg::example_pack_snippet(&spec.home_dir),
        prior: spec.prior_pack.as_ref(),
        feedback: spec.feedback.as_deref(),
    };
    let outcome = match tokio::time::timeout(spec.deadline, run_staged(llm, &input, &sink)).await {
        Ok(r) => r,
        Err(_) => Err(format!(
            "生成逾時（超過 {} 分鐘），已停止；草稿保留上一輪。可以減少團隊人數後重試，或在 config.toml 的 [experts] generate_timeout_minutes 調高上限",
            spec.deadline.as_secs().div_ceil(60)
        )),
    };
    let finished = outcome.and_then(|res| {
        sink.update(|r| r.progress.assemble = StepStatus::Running);
        assemble_round(&spec, &res.pack, &res.notes)
    });
    match finished {
        Ok(state) => {
            let status = if state.notes.is_empty() {
                JobStatus::Done
            } else {
                JobStatus::Partial
            };
            sink.update(|r| {
                r.progress.assemble = StepStatus::Done;
                r.status = status;
                r.notes = state.notes.clone();
            });
            Ok(draft_payload(&spec.home_dir, &state, status))
        }
        Err(e) => {
            sink.update(|r| {
                if r.progress.assemble == StepStatus::Running {
                    r.progress.assemble = StepStatus::Failed;
                }
                r.status = JobStatus::Failed;
                r.error = duduclaw_core::truncate_chars(&e, 800);
            });
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_search_is_granted_to_the_wiki_stage_only() {
        for stage in [Stage::Roster, Stage::Member] {
            assert_eq!(
                caps_for(stage, true).allowed_tools(),
                vec!["duduclaw_night_no_tools".to_string()],
                "{stage:?}"
            );
        }
        assert_eq!(caps_for(Stage::Wiki, true).allowed_tools(), vec!["WebSearch".to_string()]);
        assert_eq!(
            caps_for(Stage::Wiki, false).allowed_tools(),
            vec!["duduclaw_night_no_tools".to_string()]
        );
    }

    #[test]
    fn deadline_is_configurable_and_clamped() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(deadline_minutes(home.path()), DEFAULT_DEADLINE_MINUTES);
        let set = |v: &str| {
            std::fs::write(home.path().join("config.toml"), format!("[experts]\ngenerate_timeout_minutes = {v}\n"))
                .unwrap();
            deadline_minutes(home.path())
        };
        assert_eq!(set("40"), 40);
        assert_eq!(set("1"), MIN_DEADLINE_MINUTES);
        assert_eq!(set("9999"), MAX_DEADLINE_MINUTES);
        assert_eq!(set("\"x\""), DEFAULT_DEADLINE_MINUTES);
    }

    /// A model that never answers.
    struct Hang;
    #[async_trait::async_trait]
    impl StageLlm for Hang {
        async fn complete(&self, _: Stage, _: &str, _: &str) -> Result<StageReply, String> {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Err("unreachable".into())
        }
    }

    #[tokio::test]
    async fn timeout_keeps_the_previous_round() {
        let home = tempfile::tempdir().unwrap();
        let id = "abc-timeout";
        let live = eg::draft_pack_dir(home.path(), id).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("expert.toml"), "previous round").unwrap();
        let spec = JobSpec {
            home_dir: home.path().to_path_buf(),
            draft_id: id.into(),
            kind: "revise",
            req: eg::GenerateRequest {
                industry_hint: String::new(),
                description: "d".into(),
                team_size: 2,
                channels: vec![],
                research: false,
            },
            prior_state: None,
            prior_pack: None,
            feedback: Some("改".into()),
            deadline: std::time::Duration::from_millis(50),
        };
        let err = run_job(spec, &Hang).await.unwrap_err();
        assert!(err.contains("逾時") && err.contains("generate_timeout_minutes"), "{err}");
        assert_eq!(std::fs::read_to_string(live.join("expert.toml")).unwrap(), "previous round");
        let rec = super::super::job::read_job(&eg::draft_dir(home.path(), id).unwrap()).unwrap();
        assert_eq!(rec.status, JobStatus::Failed);
    }

    #[test]
    fn rollback_restores_the_previous_round() {
        let home = tempfile::tempdir().unwrap();
        let id = "abc-rollback";
        let live = eg::draft_pack_dir(home.path(), id).unwrap();
        std::fs::create_dir_all(&live).unwrap();
        std::fs::write(live.join("marker"), "old").unwrap();
        let pack: eg::GeneratedPack = serde_json::from_value(json!({
            "slug": "t-team", "display_name": "t", "description": "d",
            "agents": [{ "name": "t-front", "role": "front_desk", "soul_md": "# t" }]
        }))
        .unwrap();
        let swapped = materialize_and_swap(home.path(), id, &pack).unwrap();
        assert!(live.join("expert.toml").is_file(), "new round live");
        swapped.rollback().unwrap();
        assert_eq!(std::fs::read_to_string(live.join("marker")).unwrap(), "old");
    }
}
