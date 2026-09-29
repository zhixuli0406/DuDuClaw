//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Expert packs (專家包) — dashboard admin surface ─────────────────────
    //
    // Shared on-disk contracts + remove/hooks semantics live in
    // `crate::expert_admin` (also consumed by the `duduclaw expert` CLI).
    // Install reuses the FULL CLI pipeline (format detection, safe_zip
    // zip-slip fence + 50 MB cap, prompt-injection / skill-security scanning,
    // hook quarantine) by spawning `duduclaw expert install <path>` — the
    // installer is entangled with cli-only agent scaffolding, so a subprocess
    // is the zero-drift reuse path (same pattern as doctor_probes → mcp-server).

    pub(crate) async fn handle_experts_list(&self) -> WsFrame {
        let packs: Vec<Value> = crate::expert_admin::list_records(&self.home_dir)
            .into_iter()
            .map(|r| {
                let hooks = crate::expert_admin::read_hooks_state(&self.home_dir, &r.slug);
                json!({
                    "slug": r.slug,
                    "kind": r.kind.label(),
                    "display_name": if r.display_name.is_empty() { r.slug.clone() } else { r.display_name.clone() },
                    "version": r.version,
                    "description": r.description,
                    "agents": r.agents,
                    "skills_count": r.global_skills.len(),
                    "wiki_count": r.wiki_files.len(),
                    "installed_at": r.installed_at,
                    // null ⇒ the pack ships no managed hooks.
                    "hooks_status": hooks.as_ref().map(|h| h.status.as_str()),
                    "hooks_files": hooks.map(|h| h.files.len()).unwrap_or(0),
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "packs": packs }))
    }

    pub(crate) async fn handle_experts_install(&self, params: Value) -> WsFrame {
        let Some(path) = params.get("path").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "path parameter is required");
        };
        let src = std::path::Path::new(path);
        if !src.exists() {
            return WsFrame::error_response("", "安裝來源不存在（請重新上傳）");
        }
        let is_zip = src.is_file()
            && src
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| e.eq_ignore_ascii_case("zip"))
                .unwrap_or(false);
        if !src.is_dir() && !is_zip {
            return WsFrame::error_response("", "安裝來源必須是資料夾或 .zip 檔");
        }

        // Reuse the full CLI install pipeline via our own binary. Hooks are
        // NEVER auto-trusted from the dashboard — they land disabled with an
        // approval request (fail-closed), decided in the approval center.
        let mut args = vec!["install".into(), src.as_os_str().to_os_string()];
        match Self::attach_under_args(&params) {
            Ok(extra) => args.extend(extra),
            Err(frame) => return frame,
        }
        match self.spawn_expert_cli(&args, 300).await {
            Ok(output) => WsFrame::ok_response("", json!({ "success": true, "output": output })),
            Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
        }
    }

    /// WP-ORG: optional `attach_under` param → `--attach-under <id>` CLI args.
    /// The id shape is fenced here (fail-closed error, never silently dropped)
    /// so an arbitrary string can never become a stray flag; the CLI then
    /// validates the target actually exists before installing.
    pub(crate) fn attach_under_args(params: &Value) -> Result<Vec<std::ffi::OsString>, WsFrame> {
        match params
            .get("attach_under")
            .and_then(|v| v.as_str())
            .map(str::trim)
        {
            None | Some("") => Ok(Vec::new()),
            Some(id) if crate::premium_templates::is_safe_slug(id) => {
                Ok(vec!["--attach-under".into(), id.into()])
            }
            Some(id) => Err(WsFrame::error_response(
                "",
                &format!("attach_under '{}' 非合法 agent id", id.escape_debug()),
            )),
        }
    }

    /// Spawn our own binary's `duduclaw expert <args…>` sub-command against
    /// the gateway home — the zero-drift reuse path for the CLI install /
    /// convert pipelines (same pattern as `doctor_probes` → `mcp-server`).
    /// Returns the CJK-safe-truncated stdout tail on success.
    pub(crate) async fn spawn_expert_cli(
        &self,
        args: &[std::ffi::OsString],
        timeout_secs: u64,
    ) -> Result<String, String> {
        let bin = duduclaw_core::resolve_duduclaw_bin();
        let fut = tokio::process::Command::new(&bin)
            .arg("expert")
            .args(args)
            .env("DUDUCLAW_HOME", &self.home_dir)
            .kill_on_drop(true)
            .output();
        let output =
            match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), fut).await {
                Ok(Ok(o)) => o,
                Ok(Err(e)) => return Err(format!("子程序啟動失敗: {e}")),
                Err(_) => return Err(format!("子程序逾時（{timeout_secs} 秒），已中止")),
            };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Strip ANSI-free console tail for surfacing (CJK-safe truncation).
        let tail = |s: &str| duduclaw_core::truncate_chars(s.trim(), 1200);
        if !output.status.success() {
            let detail = if stderr.trim().is_empty() {
                &stdout
            } else {
                &stderr
            };
            return Err(tail(detail));
        }
        Ok(tail(&stdout))
    }

    pub(crate) async fn handle_experts_remove(&self, params: Value) -> WsFrame {
        let Some(slug) = params.get("slug").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "slug parameter is required");
        };
        match crate::expert_admin::remove_pack(&self.home_dir, slug).await {
            Ok(items) => WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "items": serde_json::to_value(&items).unwrap_or(Value::Null),
                }),
            ),
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_experts_hooks_apply(&self, params: Value) -> WsFrame {
        let Some(slug) = params.get("slug").and_then(|v| v.as_str()) else {
            return WsFrame::error_response("", "slug parameter is required");
        };
        use crate::expert_admin::HooksApplyOutcome as O;
        match crate::expert_admin::apply_hooks_decision(&self.home_dir, slug).await {
            Ok(O::Enabled { files }) => {
                WsFrame::ok_response("", json!({ "status": "enabled", "files": files }))
            }
            Ok(O::Disabled) => WsFrame::ok_response("", json!({ "status": "disabled" })),
            Ok(O::StillPending { approval_id }) => WsFrame::ok_response(
                "",
                json!({ "status": "pending_approval", "approval_id": approval_id }),
            ),
            Ok(O::DeniedOrExpired { status }) => {
                WsFrame::ok_response("", json!({ "status": status }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    // ── Built-in expert-pack catalog (22 industry teams → one-click install) ──

    /// `experts.catalog` — built-in industry packs available for one-click
    /// install. Fail-safe: absent premium tree ⇒ `deployed: false`, never an
    /// error. Locked license ⇒ list withheld with the upsell flag (same
    /// convention as `templates.industries`).
    pub(crate) async fn handle_experts_catalog(&self) -> WsFrame {
        let unlocked = self.premium_templates_unlocked().await;
        let dir = crate::premium_templates::find_premium_templates_dir();
        let records = crate::expert_admin::list_records(&self.home_dir);
        let catalog = crate::expert_generate::builtin_catalog(dir.as_deref(), &records);
        let deployed = catalog["deployed"].as_bool().unwrap_or(false);
        WsFrame::ok_response(
            "",
            json!({
                "deployed": deployed,
                "unlocked": unlocked,
                "present_but_locked": deployed && !unlocked,
                "packs": if unlocked { catalog["packs"].clone() } else { json!([]) },
            }),
        )
    }

    // ── Inspiration gallery (P2-b "一鍵做同款", curated-only MVP) ──

    /// `gallery.list` — read-only fan-out of the same `team.toml` task
    /// examples `experts.catalog` already reads (`expert_generate::
    /// gallery_cards`), reformatted as one showcase card per example instead
    /// of grouped under its team. No new storage: this assembles straight
    /// from the builtin catalog's source data on every call. Same
    /// license/deployment gating as `experts.catalog` (same underlying
    /// premium tree) — `present_but_locked` lets the dashboard show an
    /// upsell instead of an empty grid.
    ///
    /// Deliberately NOT included in this MVP wave (tracked in the P2-b design
    /// doc): user-submitted "我的" gallery entries from real completed goal
    /// runs — that depends on artifact objectification (I-2b), which has not
    /// shipped yet.
    pub(crate) async fn handle_gallery_list(&self) -> WsFrame {
        let unlocked = self.premium_templates_unlocked().await;
        let dir = crate::premium_templates::find_premium_templates_dir();
        let records = crate::expert_admin::list_records(&self.home_dir);
        let gallery = crate::expert_generate::gallery_cards(dir.as_deref(), &records);
        let deployed = gallery["deployed"].as_bool().unwrap_or(false);
        WsFrame::ok_response(
            "",
            json!({
                "deployed": deployed,
                "unlocked": unlocked,
                "present_but_locked": deployed && !unlocked,
                "cards": if unlocked { gallery["cards"].clone() } else { json!([]) },
            }),
        )
    }

    /// `experts.install_builtin` — convert (cached, idempotent) + install one
    /// built-in industry pack. Both steps reuse the CLI pipelines via
    /// subprocess: `expert convert-teams` then `expert install` (full
    /// security scanning, hooks fail-closed).
    pub(crate) async fn handle_experts_install_builtin(&self, params: Value) -> WsFrame {
        use crate::expert_generate as eg;
        let attach_args = match Self::attach_under_args(&params) {
            Ok(a) => a,
            Err(frame) => return frame,
        };

        // WP-ORG: `slug` (no `industry`) selects a standalone pack shipped
        // under `<premium>/experts/<slug>/` — installed straight from the
        // premium tree, no conversion step.
        let industry = params
            .get("industry")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if industry.is_empty() {
            let slug = params.get("slug").and_then(|v| v.as_str()).unwrap_or("");
            if !crate::premium_templates::is_safe_slug(slug) || slug.ends_with("-team") {
                return WsFrame::error_response("", "找不到此內建專家包");
            }
            let premium_dir = match self.premium_dir_unlocked().await {
                Ok(d) => d,
                Err(frame) => return frame,
            };
            let pack_dir = premium_dir.join("experts").join(slug);
            if !pack_dir.join("expert.toml").is_file() {
                return WsFrame::error_response("", "找不到此內建專家包");
            }
            let mut args = vec!["install".into(), pack_dir.into_os_string()];
            args.extend(attach_args);
            return match self.spawn_expert_cli(&args, 300).await {
                Ok(output) => WsFrame::ok_response(
                    "",
                    json!({ "success": true, "slug": slug, "output": output }),
                ),
                Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
            };
        }

        // Slug fence FIRST — traversal is rejected before license checks or
        // any subprocess (deterministic fail-closed ordering).
        let cache_pack = match eg::builtin_pack_cache_dir(&self.home_dir, industry) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let premium_dir = match self.premium_dir_unlocked().await {
            Ok(d) => d,
            Err(frame) => return frame,
        };
        if let Err(e) = crate::premium_templates::load_team_manifest(&premium_dir, industry) {
            warn!(industry, error = %e, "install_builtin: unknown/invalid team manifest");
            return WsFrame::error_response("", "找不到此產業的內建專家包");
        }

        // Ensure the converted cache. `convert-teams` has no per-industry
        // filter, so the whole tree is converted once (idempotent,
        // byte-deterministic) and reused afterwards.
        if !cache_pack.join("expert.toml").is_file() {
            let out_dir = eg::builtin_cache_dir(&self.home_dir);
            if let Err(e) = tokio::fs::create_dir_all(&out_dir).await {
                return WsFrame::error_response("", &format!("建立快取目錄失敗: {e}"));
            }
            let convert = self
                .spawn_expert_cli(
                    &[
                        "convert-teams".into(),
                        premium_dir.join("teams").into_os_string(),
                        "--out".into(),
                        out_dir.into_os_string(),
                    ],
                    120,
                )
                .await;
            if let Err(e) = convert {
                // The batch errs when ANY team fails; THIS industry may still
                // have converted fine — proceed only when its pack exists.
                if !cache_pack.join("expert.toml").is_file() {
                    return WsFrame::error_response("", &format!("內建包轉換失敗：{e}"));
                }
                warn!(industry, error = %e, "convert-teams reported failures; target pack present — proceeding");
            }
        }

        let mut args = vec!["install".into(), cache_pack.into_os_string()];
        args.extend(attach_args);
        match self.spawn_expert_cli(&args, 300).await {
            Ok(output) => WsFrame::ok_response(
                "",
                json!({
                    "success": true,
                    "slug": eg::builtin_pack_slug(industry),
                    "output": output,
                }),
            ),
            Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
        }
    }

    // ── LLM-guided expert-pack authoring ──────────────────────────────────

    /// One LLM round for pack generation: rotated Claude CLI (zero-tool
    /// caps) → Direct API fallback — the same call chain as
    /// `widgets.custom.generate`.
    pub(crate) async fn run_expert_generation_llm(&self, system: &str, user: &str) -> Result<String, String> {
        let caps = crate::night_llm::night_capabilities();
        let model = crate::expert_generate::GENERATE_MODEL;
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
            None, // P1/WP-3 effort: resolved from the agent dir in the callee
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
        match crate::direct_api::call_direct_api(&api_key, model, system, user, &[]).await {
            Ok(resp) if !resp.text.trim().is_empty() => Ok(resp.text),
            Ok(_) => Err("模型回傳空內容".into()),
            Err(e) => Err(duduclaw_core::truncate_chars(&e, 200)),
        }
    }

    /// Full generation round for a draft: LLM → parse → materialize →
    /// validate, with ONE auto-retry that feeds the validation errors back to
    /// the model. Returns the accepted design JSON (stored as the prior for
    /// revise rounds). Honest failure: the final validation errors are
    /// surfaced, never a half-valid draft.
    pub(crate) async fn generate_expert_draft(
        &self,
        draft_id: &str,
        req: &crate::expert_generate::GenerateRequest,
        prior_json: Option<&str>,
        feedback: Option<&str>,
    ) -> Result<String, String> {
        use crate::expert_generate as eg;
        let pack_dir = eg::draft_pack_dir(&self.home_dir, draft_id)?;
        let example = eg::example_pack_snippet(&self.home_dir);
        let mut last_errors: Vec<String> = Vec::new();

        for _attempt in 0..2 {
            let errs = (!last_errors.is_empty()).then_some(last_errors.as_slice());
            let (system, user) =
                eg::build_pack_generation_prompt(req, &example, prior_json, feedback, errs);
            let raw = self.run_expert_generation_llm(&system, &user).await?;
            let gp = match eg::parse_generated_pack(&raw) {
                Ok(g) => g,
                Err(e) => {
                    last_errors = vec![e];
                    continue;
                }
            };
            if let Err(e) = eg::materialize_draft(&pack_dir, &gp) {
                last_errors = vec![e];
                continue;
            }
            let problems = eg::validate_draft_pack(&pack_dir);
            if problems.is_empty() {
                // Store the normalized design (round-trips through the
                // schema) rather than the raw response with its prose risk.
                return Ok(serde_json::to_string(&gp).unwrap_or(raw));
            }
            last_errors = problems;
        }
        Err(format!(
            "草稿未通過驗證（已自動重試 1 次）：{}",
            duduclaw_core::truncate_chars(&last_errors.join("；"), 600)
        ))
    }

    /// `experts.generate` — guided-form → LLM draft under
    /// `<home>/tmp/expert-drafts/<id>/pack/`. Nothing is installed here; the
    /// draft previews client-side and `experts.install_draft` runs the full
    /// security pipeline when the admin accepts.
    pub(crate) async fn handle_experts_generate(&self, params: Value) -> WsFrame {
        use crate::expert_generate as eg;
        // Opportunistic 24 h sweep (same convention as expert-uploads).
        eg::cleanup_expired_drafts(&self.home_dir);

        let req: eg::GenerateRequest = match serde_json::from_value(params) {
            Ok(r) => r,
            Err(_) => {
                return WsFrame::error_response("", "請描述這個專家包要解決什麼問題");
            }
        };
        if let Err(e) = eg::validate_generate_request(&req) {
            return WsFrame::error_response("", &e);
        }

        let draft_id = uuid::Uuid::new_v4().to_string();
        match self
            .generate_expert_draft(&draft_id, &req, None, None)
            .await
        {
            Ok(accepted) => {
                let now = crate::expert_admin::now_iso();
                let state = eg::DraftState {
                    draft_id: draft_id.clone(),
                    request: req,
                    rounds: 1,
                    created_at: now.clone(),
                    updated_at: now,
                    last_generation: accepted,
                };
                if let Err(e) = eg::write_draft_state(&self.home_dir, &state) {
                    return WsFrame::error_response("", &e);
                }
                WsFrame::ok_response(
                    "",
                    json!({
                        "draft_id": state.draft_id,
                        "rounds": state.rounds,
                        "rounds_left": state.rounds_left(),
                        "preview": eg::draft_preview_json(&self.home_dir, &state),
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("專家包生成失敗：{e}")),
        }
    }

    /// `experts.generate_revise` — regenerate a draft with the admin's
    /// feedback (prior design replayed as DATA). Capped at
    /// [`crate::expert_generate::MAX_GENERATE_ROUNDS`] total rounds.
    pub(crate) async fn handle_experts_generate_revise(&self, params: Value) -> WsFrame {
        use crate::expert_generate as eg;
        let draft_id = params
            .get("draft_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let feedback = params
            .get("feedback")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if feedback.is_empty() {
            return WsFrame::error_response("", "請說明想修改哪裡");
        }
        if feedback.chars().count() > eg::MAX_DESCRIPTION_CHARS {
            return WsFrame::error_response(
                "",
                &format!("回饋最長 {} 字", eg::MAX_DESCRIPTION_CHARS),
            );
        }
        let mut state = match eg::read_draft_state(&self.home_dir, draft_id) {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if !state.can_revise() {
            return WsFrame::error_response(
                "",
                &format!(
                    "已達重新生成上限（{} 輪）。請直接安裝或重新開始。",
                    eg::MAX_GENERATE_ROUNDS
                ),
            );
        }

        let prior = state.last_generation.clone();
        let req = state.request.clone();
        match self
            .generate_expert_draft(&state.draft_id, &req, Some(&prior), Some(feedback))
            .await
        {
            Ok(accepted) => {
                state.rounds += 1;
                state.updated_at = crate::expert_admin::now_iso();
                state.last_generation = accepted;
                if let Err(e) = eg::write_draft_state(&self.home_dir, &state) {
                    return WsFrame::error_response("", &e);
                }
                WsFrame::ok_response(
                    "",
                    json!({
                        "draft_id": state.draft_id,
                        "rounds": state.rounds,
                        "rounds_left": state.rounds_left(),
                        "preview": eg::draft_preview_json(&self.home_dir, &state),
                    }),
                )
            }
            Err(e) => WsFrame::error_response("", &format!("重新生成失敗：{e}")),
        }
    }

    /// `experts.install_draft` — install a generated draft through the FULL
    /// CLI security pipeline (LLM output is external content; scanning is
    /// never skipped). The draft dir is cleaned up on success.
    pub(crate) async fn handle_experts_install_draft(&self, params: Value) -> WsFrame {
        use crate::expert_generate as eg;
        let draft_id = params
            .get("draft_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let pack_dir = match eg::draft_pack_dir(&self.home_dir, draft_id) {
            Ok(d) => d,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if !pack_dir.join("expert.toml").is_file() {
            return WsFrame::error_response("", "找不到這份草稿（可能已過期清除，請重新生成）");
        }
        // Defense in depth: the no-hooks rule is re-checked at the install
        // boundary, not just at generation time.
        if let Err(e) = eg::ensure_no_hooks(&pack_dir) {
            return WsFrame::error_response("", &e);
        }
        let mut args = vec!["install".into(), pack_dir.into_os_string()];
        match Self::attach_under_args(&params) {
            Ok(extra) => args.extend(extra),
            Err(frame) => return frame,
        }
        match self.spawn_expert_cli(&args, 300).await {
            Ok(output) => {
                if let Ok(dir) = eg::draft_dir(&self.home_dir, draft_id) {
                    let _ = tokio::fs::remove_dir_all(&dir).await;
                }
                WsFrame::ok_response("", json!({ "success": true, "output": output }))
            }
            Err(e) => WsFrame::error_response("", &format!("安裝失敗：{e}")),
        }
    }
}
