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
use crate::expert_generate_staged::{self as staged, job};

use staged::runner::{JobSpec, RotatedStageLlm, draft_payload, run_job};

impl MethodHandler {
    fn generation_deadline(&self) -> std::time::Duration {
        std::time::Duration::from_secs(staged::runner::deadline_minutes(&self.home_dir) * 60)
    }

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
        let slot = match job::reserve(&draft_id, job::SlotKind::Generate) {
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
            deadline: self.generation_deadline(),
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
        let slot = match job::reserve(draft_id, job::SlotKind::Generate) {
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
            deadline: self.generation_deadline(),
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
        let outcome = job::abort(draft_id);
        WsFrame::ok_response(
            "",
            json!({
                "cancelled": outcome == job::AbortOutcome::Cancelled,
                // `installing`: installs cannot be cancelled; `not_running`:
                // nothing was running in this gateway process.
                "state": match outcome {
                    job::AbortOutcome::Cancelled => "cancelled",
                    job::AbortOutcome::Installing => "installing",
                    job::AbortOutcome::NotRunning => "not_running",
                },
            }),
        )
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
        let _slot = match job::reserve(draft_id, job::SlotKind::Install) {
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
        // Pages written with web search may carry outside content: the
        // operator must confirm they reviewed them (fail closed).
        if staged::report::has_web_research_pages(pack_dir)
            && params.get("reviewed_web_pages").and_then(|v| v.as_bool()) != Some(true)
        {
            return WsFrame::error_response(
                "",
                "這份草稿有用網路搜尋寫的知識庫頁面，請先在預覽中看過並勾選確認後再安裝",
            );
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
                let placement = staged::report::placement_report(
                    &self.home_dir,
                    pack_dir,
                    record.as_ref(),
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
