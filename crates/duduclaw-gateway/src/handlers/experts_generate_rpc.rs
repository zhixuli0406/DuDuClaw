//! LLM-guided expert-team authoring RPCs: `experts.generate`,
//! `experts.generate_revise`, `experts.generate_status`,
//! `experts.generate_cancel`, `experts.install_draft`.
//!
//! Generation is staged (`crate::expert_generate_staged`) and can take
//! several minutes, so it runs as a background task per draft. With
//! `background: true` the RPC returns the draft id at once and the dashboard
//! polls `experts.generate_status`; without it the RPC waits for the task
//! (the pre-staged contract, kept for scripts). Install always goes through
//! the full CLI security pipeline and reports what landed.

#[allow(unused_imports)]
use super::*;

use crate::expert_generate as eg;
use crate::expert_generate_staged::{self as staged, Stage, StageLlm, job};

/// Whole-job deadline (all stages). A circuit breaker, not an expectation:
/// a full 8-member team at concurrency 2 is ~5 waves of model calls.
const JOB_DEADLINE_SECS: u64 = 25 * 60;
/// Direct API per-request deadline for the long content stages.
const STAGE_HTTP_TIMEOUT_SECS: u64 = 300;

/// Rotated Claude CLI (subscription first) → Direct API fallback, per stage.
/// Zero tools, except the knowledge-base stage may use `WebSearch` when the
/// operator opted in (`research`).
struct RotatedStageLlm {
    home_dir: std::path::PathBuf,
    research: bool,
}

#[async_trait::async_trait]
impl StageLlm for RotatedStageLlm {
    async fn complete(&self, stage: Stage, system: &str, user: &str) -> Result<String, String> {
        let caps = if self.research && stage == Stage::Wiki {
            crate::night_llm::research_capabilities()
        } else {
            crate::night_llm::night_capabilities()
        };
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
            Ok(text) if !text.trim().is_empty() => return Ok(text),
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
            Ok(resp) if !resp.text.trim().is_empty() => Ok(resp.text),
            Ok(_) => Err("模型回傳空內容".into()),
            Err(e) => Err(duduclaw_core::truncate_chars(&e, 200)),
        }
    }
}

/// Everything one background job needs (owned: it outlives the RPC).
struct JobSpec {
    home_dir: std::path::PathBuf,
    draft_id: String,
    kind: &'static str,
    req: eg::GenerateRequest,
    /// `Some` on revise: the state the new round builds on.
    prior_state: Option<eg::DraftState>,
    prior_pack: Option<eg::GeneratedPack>,
    feedback: Option<String>,
}

/// Materialize into `<draft>/pack-next`, validate, then swap it in — a
/// failed or cancelled round never destroys the previous draft pack.
fn materialize_and_swap(home: &std::path::Path, draft_id: &str, pack: &eg::GeneratedPack) -> Result<(), String> {
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
        // Roll back so the previous round stays installable.
        if had_live {
            let _ = std::fs::rename(&prev, &live);
        }
        return Err(format!("替換草稿失敗: {e}"));
    }
    if had_live && let Err(e) = std::fs::remove_dir_all(&prev) {
        warn!(error = %e, "expert draft: could not remove the previous round");
    }
    Ok(())
}

/// Result payload shared by the generate/revise/status responses.
fn draft_payload(home: &std::path::Path, state: &eg::DraftState, status: job::JobStatus) -> Value {
    json!({
        "draft_id": state.draft_id,
        "status": status,
        "rounds": state.rounds,
        "rounds_left": state.rounds_left(),
        "notes": state.notes,
        "preview": eg::draft_preview_json(home, state),
    })
}

async fn run_job(spec: JobSpec, llm: &dyn StageLlm) -> Result<Value, String> {
    let draft_dir = eg::draft_dir(&spec.home_dir, &spec.draft_id)?;
    let sink = job::ProgressSink::new(draft_dir, job::JobRecord::new(spec.kind));
    sink.update(|_| {});
    let input = staged::StagedRequest {
        req: &spec.req,
        installed_example: eg::example_pack_snippet(&spec.home_dir),
        prior: spec.prior_pack.as_ref(),
        feedback: spec.feedback.as_deref(),
    };
    let deadline = std::time::Duration::from_secs(JOB_DEADLINE_SECS);
    let outcome = match tokio::time::timeout(deadline, staged::run_staged(llm, &input, &sink)).await {
        Ok(r) => r,
        Err(_) => Err(format!("生成逾時（超過 {} 分鐘），已停止", JOB_DEADLINE_SECS / 60)),
    };
    let finished = outcome.and_then(|res| {
        sink.update(|r| r.progress.assemble = job::StepStatus::Running);
        materialize_and_swap(&spec.home_dir, &spec.draft_id, &res.pack)?;
        let now = crate::expert_admin::now_iso();
        let state = match spec.prior_state.clone() {
            Some(prev) => eg::DraftState {
                rounds: prev.rounds + 1,
                updated_at: now,
                last_generation: serde_json::to_string(&res.pack).unwrap_or_default(),
                notes: res.notes.clone(),
                ..prev
            },
            None => eg::DraftState {
                draft_id: spec.draft_id.clone(),
                request: spec.req.clone(),
                rounds: 1,
                created_at: now.clone(),
                updated_at: now,
                last_generation: serde_json::to_string(&res.pack).unwrap_or_default(),
                notes: res.notes.clone(),
            },
        };
        eg::write_draft_state(&spec.home_dir, &state)?;
        Ok(state)
    });
    match finished {
        Ok(state) => {
            let status = if state.notes.is_empty() {
                job::JobStatus::Done
            } else {
                job::JobStatus::Partial
            };
            sink.update(|r| {
                r.progress.assemble = job::StepStatus::Done;
                r.status = status;
                r.notes = state.notes.clone();
            });
            Ok(draft_payload(&spec.home_dir, &state, status))
        }
        Err(e) => {
            sink.update(|r| {
                if r.progress.assemble == job::StepStatus::Running {
                    r.progress.assemble = job::StepStatus::Failed;
                }
                r.status = job::JobStatus::Failed;
                r.error = duduclaw_core::truncate_chars(&e, 800);
            });
            Err(e)
        }
    }
}

impl MethodHandler {
    /// Spawn one job (slot already reserved by the caller), then either
    /// return at once (`background`) or wait for it.
    async fn start_generation_job(
        &self,
        slot: job::Slot,
        spec: JobSpec,
        background: bool,
        label: &str,
    ) -> WsFrame {
        let draft_id = spec.draft_id.clone();
        let llm = RotatedStageLlm {
            home_dir: self.home_dir.clone(),
            research: spec.req.research,
        };
        // Write the initial record before spawning so a status poll that
        // races the task already sees `running`.
        let dir = match eg::draft_dir(&self.home_dir, &draft_id) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if let Err(e) = job::write_job(&dir, &job::JobRecord::new(spec.kind)) {
            return WsFrame::error_response("", &e); // slot dropped here
        }
        // The slot travels with the task: whether the task returns, is
        // aborted or panics, dropping it frees the draft and finalizes a
        // record still marked `running`.
        let slot = slot.finalize_record_in(dir);
        let (tx, rx) = tokio::sync::oneshot::channel::<job::Slot>();
        let handle = tokio::spawn(async move {
            let Ok(_slot) = rx.await else {
                return Err("工作未啟動".to_string());
            };
            run_job(spec, &llm).await
        });
        slot.attach(handle.abort_handle());
        if tx.send(slot).is_err() {
            return WsFrame::error_response("", &format!("{label}：工作未啟動"));
        }
        if background {
            let record = eg::draft_dir(&self.home_dir, &draft_id)
                .ok()
                .and_then(|d| job::read_job(&d));
            return WsFrame::ok_response(
                "",
                json!({ "draft_id": draft_id, "status": "running", "job": record }),
            );
        }
        match handle.await {
            Ok(Ok(payload)) => WsFrame::ok_response("", payload),
            Ok(Err(e)) => WsFrame::error_response("", &format!("{label}：{e}")),
            Err(e) => WsFrame::error_response("", &format!("{label}：工作中止（{e}）")),
        }
    }

    /// `experts.generate` — guided form → staged LLM draft under
    /// `<home>/tmp/expert-drafts/<id>/pack/`. Nothing is installed here.
    pub(crate) async fn handle_experts_generate(&self, params: Value) -> WsFrame {
        // Opportunistic 24 h sweep (same convention as expert-uploads).
        eg::cleanup_expired_drafts(&self.home_dir);
        let background = params.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
        let req: eg::GenerateRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(_) => return WsFrame::error_response("", "請描述這個專家包要解決什麼問題"),
        };
        if let Err(e) = eg::validate_generate_request(&req) {
            return WsFrame::error_response("", &e);
        }
        let draft_id = uuid::Uuid::new_v4().to_string();
        let slot = match job::reserve(&draft_id) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", e.message()),
        };
        let spec = JobSpec {
            home_dir: self.home_dir.clone(),
            draft_id,
            kind: "generate",
            req,
            prior_state: None,
            prior_pack: None,
            feedback: None,
        };
        self.start_generation_job(slot, spec, background, "專家包生成失敗").await
    }

    /// `experts.generate_revise` — regenerate every stage with the admin's
    /// feedback (prior draft replayed as DATA). Capped at
    /// [`eg::MAX_GENERATE_ROUNDS`] total rounds.
    pub(crate) async fn handle_experts_generate_revise(&self, params: Value) -> WsFrame {
        let background = params.get("background").and_then(|v| v.as_bool()).unwrap_or(false);
        let draft_id = params.get("draft_id").and_then(|v| v.as_str()).unwrap_or("");
        let feedback = params.get("feedback").and_then(|v| v.as_str()).unwrap_or("").trim();
        if feedback.is_empty() {
            return WsFrame::error_response("", "請說明想修改哪裡");
        }
        if feedback.chars().count() > eg::MAX_DESCRIPTION_CHARS {
            return WsFrame::error_response("", &format!("回饋最長 {} 字", eg::MAX_DESCRIPTION_CHARS));
        }
        if !eg::is_safe_draft_id(draft_id) {
            return WsFrame::error_response("", "非法的草稿編號");
        }
        // Reserve first, then read: a round that finished in between can
        // never be built on stale state (round counter, prior content).
        let slot = match job::reserve(draft_id) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", e.message()),
        };
        let state = match eg::read_draft_state(&self.home_dir, draft_id) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if !state.can_revise() {
            return WsFrame::error_response(
                "",
                &format!("已達重新生成上限（{} 輪）。請直接安裝或重新開始。", eg::MAX_GENERATE_ROUNDS),
            );
        }
        // A prior that no longer parses is not fatal: the round still runs
        // with the feedback, just without the old content as reference.
        let prior_pack = serde_json::from_str::<eg::GeneratedPack>(&state.last_generation).ok();
        let spec = JobSpec {
            home_dir: self.home_dir.clone(),
            draft_id: state.draft_id.clone(),
            kind: "revise",
            req: state.request.clone(),
            prior_state: Some(state),
            prior_pack,
            feedback: Some(feedback.to_string()),
        };
        self.start_generation_job(slot, spec, background, "重新生成失敗").await
    }

    /// `experts.generate_status` — progress of the draft's latest job, plus
    /// the draft preview once it finished.
    pub(crate) async fn handle_experts_generate_status(&self, params: Value) -> WsFrame {
        let draft_id = params.get("draft_id").and_then(|v| v.as_str()).unwrap_or("");
        let dir = match eg::draft_dir(&self.home_dir, draft_id) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let state = eg::read_draft_state(&self.home_dir, draft_id).ok();
        let Some(record) = job::effective_job(&dir, draft_id) else {
            // Draft from before background jobs existed: finished by definition.
            return match state {
                Some(s) => WsFrame::ok_response(
                    "",
                    draft_payload(&self.home_dir, &s, job::JobStatus::Done),
                ),
                None => WsFrame::error_response("", "找不到這份草稿（可能已過期清除，請重新生成）"),
            };
        };
        let mut out = json!({
            "draft_id": draft_id,
            "status": record.status,
            "job": record,
        });
        if let Some(s) = state.filter(|_| record.status.is_terminal())
            && let Value::Object(extra) = draft_payload(&self.home_dir, &s, record.status)
            && let Value::Object(map) = &mut out
        {
            for (k, v) in extra {
                if k != "status" {
                    map.insert(k, v);
                }
            }
        }
        WsFrame::ok_response("", out)
    }

    /// `experts.generate_cancel` — stop the draft's running job. The draft
    /// keeps its previous accepted round (if any).
    pub(crate) async fn handle_experts_generate_cancel(&self, params: Value) -> WsFrame {
        let draft_id = params.get("draft_id").and_then(|v| v.as_str()).unwrap_or("");
        if !eg::is_safe_draft_id(draft_id) {
            return WsFrame::error_response("", "非法的草稿編號");
        }
        // The job's own slot records `cancelled` when the aborted task is
        // dropped; this handler never writes the record itself.
        let cancelled = job::abort(draft_id);
        WsFrame::ok_response("", json!({ "cancelled": cancelled }))
    }

    /// `experts.install_draft` — install a generated draft through the FULL
    /// CLI security pipeline (LLM output is external content; scanning is
    /// never skipped), then report what landed: per-member skills and
    /// CONTRACT.toml, and every wiki page (installed / same-name page kept /
    /// blocked by the content scan). The draft dir is cleaned up on success.
    pub(crate) async fn handle_experts_install_draft(&self, params: Value) -> WsFrame {
        let draft_id = params.get("draft_id").and_then(|v| v.as_str()).unwrap_or("");
        let pack_dir = match eg::draft_pack_dir(&self.home_dir, draft_id) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        // Hold the draft's slot for the whole install, so a revise cannot
        // swap the pack while the installer is reading it.
        let _slot = match job::reserve(draft_id) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", e.message()),
        };
        self.install_reserved_draft(draft_id, &pack_dir, &params).await
    }

    async fn install_reserved_draft(
        &self,
        draft_id: &str,
        pack_dir: &std::path::Path,
        params: &Value,
    ) -> WsFrame {
        if !pack_dir.join("expert.toml").is_file() {
            return WsFrame::error_response("", "找不到這份草稿（可能已過期清除，請重新生成）");
        }
        // Defense in depth: the no-hooks rule is re-checked at the install
        // boundary, not just at generation time.
        if let Err(e) = eg::ensure_no_hooks(pack_dir) {
            return WsFrame::error_response("", &e);
        }
        let mut args = vec!["install".into(), pack_dir.as_os_str().to_os_string()];
        match Self::attach_under_args(params) {
            Ok(extra) => args.extend(extra),
            Err(frame) => return frame,
        }
        let preexisting = staged::report::existing_wiki_targets(&self.home_dir, pack_dir);
        // The slug the installer records under is the manifest's own name.
        let slug = std::fs::read_to_string(pack_dir.join("expert.toml"))
            .ok()
            .and_then(|raw| raw.parse::<toml::Table>().ok())
            .and_then(|t| {
                t.get("expert")
                    .and_then(|e| e.get("name"))
                    .and_then(|n| n.as_str())
                    .map(str::to_string)
            });
        match self.spawn_pack_cli(&args, 300).await {
            Ok(output) => {
                let record = slug.and_then(|s| crate::expert_admin::read_record(&self.home_dir, &s));
                let (agents, wiki) = record
                    .map(|r| (r.agents, r.wiki_files))
                    .unwrap_or_default();
                let placement = staged::report::placement_report(
                    &self.home_dir,
                    pack_dir,
                    &agents,
                    &wiki,
                    &preexisting,
                );
                if let Ok(dir) = eg::draft_dir(&self.home_dir, draft_id) {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                }
                WsFrame::ok_response(
                    "",
                    json!({ "success": true, "output": output, "placement": placement }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
        }
    }
}
