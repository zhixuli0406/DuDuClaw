//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;
use crate::google_signin::avatar::AvatarCreation;

impl MethodHandler {
    /// `templates.create_agent` — create ONE agent from a (possibly edited)
    /// role template. Admin-edited SOUL.md is taken as-is; edited
    /// CONTRACT.toml / agent.toml must still parse (fail-closed: an invalid
    /// document is rejected, nothing is written). `[agent].name` is always
    /// forced to the final agent name so the directory and config agree.
    pub(crate) async fn handle_templates_create_agent(&self, params: Value, caller: &UserContext) -> WsFrame {
        use crate::premium_templates as pt;
        // Native Linux protected mode (SR2 1c): before the license/premium
        // read; `create_template_with_skills` refuses again for every caller.
        // Only where the isolation architecture is installed (v1.80 otherwise).
        if let Some(refused) = super::isolation_employee_rpc::protected_create_refusal(
            crate::runtime_readiness::isolation_active()) {
            return refused;
        }
        let role_id = params.get("role_id").and_then(|v| v.as_str()).unwrap_or("");
        if role_id.is_empty() {
            return WsFrame::error_response("", "role_id is required");
        }
        let premium_dir = match self.premium_dir_unlocked().await {
            Ok(d) => d,
            Err(frame) => return frame,
        };
        let assembled = match self
            .assemble_template_role(&premium_dir, &params, role_id)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(role_id, error = %e, "templates.create_agent: assembly failed");
                return WsFrame::error_response(
                    "",
                    &format!("無法組裝角色板模：{}", scrub_premium_path(&e, &premium_dir)),
                );
            }
        };

        self.create_assembled_template_agent(&params, caller, &premium_dir, role_id, &assembled).await
    }

    /// The real post-license writer. Kept owner-private so owned fixtures can
    /// test the commit without faking a global license or reading host assets.
    pub(super) async fn create_assembled_template_agent(
        &self, params: &Value, caller: &UserContext, premium_dir: &Path,
        role_id: &str, assembled: &crate::premium_templates::AssembledRole,
    ) -> WsFrame {
        self.create_template_with_skills(params, caller, premium_dir, role_id, assembled, None, true, true, &[], None).await
    }

    /// Shared commit path for company and bundled templates. Skills are written
    /// before the registry-visible configuration, with the same rollback boundary.
    ///
    /// `batch_admission`: `Some` when the caller (`templates.apply` for a
    /// team) already holds the seat admission for its whole batch and
    /// admitted every member under it; `None` takes the admission here for
    /// this one employee and holds it to the publish (RM-149).
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn create_template_with_skills(
        &self, params: &Value, caller: &UserContext, premium_dir: &Path,
        role_id: &str, assembled: &crate::premium_templates::AssembledRole,
        skills: Option<&std::collections::BTreeMap<String, String>>, premium: bool, install_evals: bool, disabled_skills: &[String],
        batch_admission: Option<&duduclaw_core::agent_seats::SeatAdmission>,
    ) -> WsFrame {
        self.create_template_with_skills_inner(params, Some(caller), premium_dir, role_id,
            assembled, skills, premium, install_evals, disabled_skills, batch_admission,
            AvatarCreation::None).await
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn create_template_with_skills_inner(
        &self, params: &Value, caller: Option<&UserContext>, premium_dir: &Path,
        role_id: &str, assembled: &crate::premium_templates::AssembledRole,
        skills: Option<&std::collections::BTreeMap<String, String>>, premium: bool, install_evals: bool, disabled_skills: &[String],
        batch_admission: Option<&duduclaw_core::agent_seats::SeatAdmission>, avatar: AvatarCreation<'_>,
    ) -> WsFrame {
        use crate::premium_templates as pt;
        // Native Linux protected mode (SR2 1c): a protected employee exists
        // only through an approved Employee proposal. Every template writer
        // (role, company, bundled, library) commits here, so refuse first.
        if let Some(refused) = super::isolation_employee_rpc::protected_create_refusal(
            crate::runtime_readiness::isolation_active()) {
            return refused;
        }
        if !avatar.is_signin() && !caller.is_some_and(UserContext::is_admin) {
            return WsFrame::error_response("", "Administrator required");
        }
        let avatar_db = if avatar.member().is_some() {
            let Some(db) = self.user_db.read().await.clone() else {
                return WsFrame::error_response("", "無法讀取成員資料，未建立分身。");
            };
            Some(db)
        } else { None };
        if let Err(error) = self.validate_template_policy(assembled) {
            return WsFrame::error_response("", &error);
        }
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or(&assembled.name)
            .to_string();
        if !is_valid_agent_id(&name) {
            return WsFrame::error_response(
                "",
                "Agent name must be lowercase alphanumeric with hyphens, max 64 chars",
            );
        }
        if duduclaw_core::is_reserved_agent_id(&name) {
            return WsFrame::error_response("", "Reserved employee name");
        }
        if self.registry.read().await.list().iter().any(|a| {
            a.config.agent.name == name || a.dir.file_name().and_then(|v| v.to_str()) == Some(name.as_str())
        }) {
            return WsFrame::error_response("", "Employee name already exists");
        }
        if let Some(skills) = skills {
            for key in skills.keys() {
                if crate::company_templates::validate_skill_name(key).is_err() {
                    return WsFrame::error_response("", "Invalid template skill name");
                }
            }
        }
        let display_name = params
            .get("display_name")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let trigger = params.get("trigger").and_then(|v| v.as_str()).unwrap_or("");

        // Optional org placement overrides. Absent/empty ⇒ keep the template's
        // wiring (workers already report to the pack's front desk). A non-empty
        // `reports_to` must name an existing agent; `department` follows the
        // WP7 allowlist (validated again inside the identity patch).
        let reports_to_override = params
            .get("reports_to")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(rt) = &reports_to_override {
            if *rt == name {
                return WsFrame::error_response("", "上級不能是自己");
            }
            let exists = self
                .registry
                .read()
                .await
                .list()
                .iter()
                .any(|a| a.config.agent.name == *rt);
            if !exists {
                return WsFrame::error_response("", &format!("上級 AI 員工「{rt}」不存在"));
            }
        }
        let department_override = params
            .get("department")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        // SOUL.md: the admin's edited prompt wins; empty falls back to the
        // template. Cap defensively (CJK-safe char count).
        let soul_md = params
            .get("soul_md")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| assembled.soul_md.clone());
        if soul_md.chars().count() > 100_000 {
            return WsFrame::error_response("", "SOUL.md 內容過長（上限 100k 字元）");
        }

        // CONTRACT.toml: must parse or we refuse to write anything.
        let contract_toml = params
            .get("contract_toml")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| assembled.contract_toml.clone());
        if contract_toml.chars().count() > 64_000 {
            return WsFrame::error_response("", "CONTRACT.toml 內容過長（上限 64k 字元）");
        }
        if let Err(e) = contract_toml.parse::<toml::Table>() {
            return WsFrame::error_response("", &format!("CONTRACT.toml 格式錯誤，未寫入：{e}"));
        }

        // Inspect every document before directory creation, demotion or writes.
        // Legacy callers seed bundled skills; scan those exact documents too.
        let bundled;
        let documents = match skills {
            Some(value) => value,
            None => { bundled = super::templates_library::bundled_skills(); &bundled }
        };
        if let Err(error) = crate::company_templates::scan_documents(&soul_md, &contract_toml, documents) {
            return WsFrame::error_response("", &error);
        }

        // agent.toml: parse-validate + force identity fields.
        let agent_src = params
            .get("agent_toml")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| assembled.agent_toml.clone());
        if agent_src.chars().count() > 64_000 {
            return WsFrame::error_response("", "agent.toml 內容過長（上限 64k 字元）");
        }
        let agent_toml = match pt::override_agent_identity(
            &agent_src,
            &name,
            display_name,
            trigger,
            reports_to_override.as_deref(),
            department_override.as_deref(),
        ) {
            Ok(t) => t,
            Err(e) => {
                return WsFrame::error_response("", &format!("agent.toml 格式錯誤，未寫入：{e}"));
            }
        };
        let (agent_toml, alignment) = match prepare_template_runtime(&self.home_dir, &agent_toml) {
            Ok(value) => value,
            Err(error) => return WsFrame::error_response("", &error),
        };
        let parsed_agent: toml::Table = match agent_toml.parse() {
            Ok(t) => t,
            Err(e) => {
                return WsFrame::error_response("", &format!("agent.toml 格式錯誤，未寫入：{e}"));
            }
        };
        if let Err(e) = crate::product_policy::load(&self.home_dir)
            .and_then(|policy| crate::product_policy::validate_table(policy, &parsed_agent)) {
            return WsFrame::error_response("", &e);
        }
        let final_role = parsed_agent
            .get("agent")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("specialist")
            .to_string();

        // Licence seats (RM-149): the shared on-disk rule — deleted employees
        // free their seat, archived ones keep it — decided under the
        // cross-process seat admission lock, held until the publish below
        // (lock order: seat admission, then the org lock). A team batch
        // passes its own held admission (all members admitted at once).
        let own_admission = match batch_admission {
            Some(_) => None,
            None => match self.admit_agent_seats(usize::from(avatar.member().is_none()), None).await {
                Ok(admission) => Some(admission),
                Err(refusal) => return super::agents_basic::seat_refusal_frame(refusal),
            },
        };

        let seat_cap = self.effective_agent_cap().await;
        let agents_dir = self.registry.read().await.agents_dir().to_path_buf();
        let agent_dir = agents_dir.join(&name);

        // RM-027 round 4: take the global org lock at the START and hold it
        // to the publish. The team member is built in an invisible staging
        // directory next to its final path and renamed into place only after
        // its authority row committed; an authority refusal (corrupt store,
        // a cycle) fails the whole create — no mirror-governed employee.
        let org = match super::org_change_rpc::lock_org(&self.home_dir).await {
            Ok(org) => org,
            Err(e) => return WsFrame::error_response("", &e),
        };
        if let Err(e) = tokio::fs::create_dir_all(&agents_dir).await {
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }
        let staged = match duduclaw_core::org_store::StagedAgentDir::create(&org, &agent_dir, &name) {
            Ok(staged) => staged,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return WsFrame::error_response("", &format!("Agent '{name}' already exists"));
            }
            Err(e) => return WsFrame::error_response("", &format!("Failed to create directory: {e}")),
        };
        // Dropping `staged` on any early return removes the staging dir.
        let stage_dir = staged.path().to_path_buf();
        let publication = match avatar.publication(&stage_dir, &agent_dir) {
            Ok(proof) => proof,
            Err(e) => return WsFrame::error_response("", &format!("Failed to create directory: {e}")),
        };

        // Only onboarding uses this guard; existing template callers keep
        // their original rollback behaviour. Restore only switches we added.
        let mut signin_skills = if avatar.is_signin() && !disabled_skills.is_empty() {
            let before = duduclaw_agent::skill_state::read_disabled(&self.home_dir, &name);
            if before.error().is_some() {
                return WsFrame::error_response("", "Unable to preserve disabled template skills");
            }
            Some(SigninSkillRollback {
                home: self.home_dir.clone(), name: name.clone(),
                added: disabled_skills.iter().filter(|key| !before.is_disabled(key)).cloned().collect(),
                written: false, committed: false,
            })
        } else { None };

        // Write SOUL.md / CONTRACT.toml / extras / agent.toml into the
        // staging directory; the directory rename after the authority commit
        // is the single registry-visible commit point. Any failure before it
        // removes the whole staging directory — no half-created agent (one
        // missing its reviewed CONTRACT) can ever be scanned in.
        let write_result: Result<Option<String>, String> = async {
            tokio::fs::create_dir_all(stage_dir.join("SKILLS"))
                .await
                .map_err(|e| format!("Failed to create directory: {e}"))?;
            Self::seed_builtin_skills(&self.home_dir);
            if let Some(skills) = skills {
                let bundled = super::templates_library::bundled_skills();
                for (name, content) in skills {
                    // Pristine creation defaults come from the company layer.
                    // Keep custom overrides and explicitly selected optional skills.
                    let company_default = name == "connect-google"
                        || duduclaw_agent::builtin_skills::BUILTIN_SKILL_FILES.iter()
                            .any(|(builtin, _)| name == builtin);
                    if company_default && bundled.get(name) == Some(content) { continue; }
                    let dir = stage_dir.join("SKILLS").join(name);
                    tokio::fs::create_dir(&dir).await.map_err(|_| "Unable to create template skill")?;
                    tokio::fs::write(dir.join("SKILL.md"), content).await.map_err(|_| "Unable to write template skill")?;
                }
            }
            tokio::fs::write(stage_dir.join(".template-origin.json"),
                serde_json::to_vec(&json!({"premium":premium})).map_err(|_| "Invalid template origin")?)
                .await.map_err(|_| "Unable to write template origin")?;
            tokio::fs::write(stage_dir.join("SOUL.md"), &soul_md)
                .await
                .map_err(|e| format!("Failed to write SOUL.md: {e}"))?;
            tokio::fs::write(stage_dir.join("CONTRACT.toml"), &contract_toml)
                .await
                .map_err(|e| format!("Failed to write CONTRACT.toml: {e}"))?;

            // Front desk carries the industry pack's knowledge extras
            // (FAQ.json + wiki/). Best-effort: a copy failure is surfaced as a
            // warning but doesn't abort the create.
            let mut extras_warning: Option<String> = None;
            if let Some(src) = &assembled.extras_dir {
                if let Err(e) = copy_template_extras(src, &stage_dir).await {
                    warn!(agent = %name, error = %e, "template extras copy failed");
                    extras_warning = Some("知識檔（FAQ/wiki）複製失敗，可稍後手動補上".to_string());
                }
            }

            if !disabled_skills.is_empty() {
                if let Some(rollback) = &mut signin_skills { rollback.written = true; }
                duduclaw_agent::skill_state::set_disabled(&self.home_dir, &name, disabled_skills, true)
                    .map_err(|_| "Unable to preserve disabled template skills")?;
            }

            // Validate the final serialized profile, including caller edits and
            // runtime alignment, against a new employee's absent Odoo baseline.
            super::config_commit::refuse_odoo_agent_widening_text_edit("", &agent_toml)?;
            tokio::fs::write(stage_dir.join("agent.toml"), &agent_toml)
                .await
                .map_err(|e| format!("Failed to write agent.toml: {e}"))?;
            Ok(extras_warning)
        }
        .await;
        let extras_warning = match write_result {
            Ok(w) => w,
            Err(e) => return WsFrame::error_response("", &e),
        };

        // Creating a new main (CEO / front desk) demotes the current one —
        // only after every write succeeded and right before the publish, so
        // a doomed create can no longer demote the live main as a side
        // effect (org lock first, then the agent.toml lock).
        let demotion = if final_role == "main" {
            match self.demote_current_main(&name).await {
                Ok(d) => d,
                Err(e) => return WsFrame::error_response("", &e),
            }
        } else {
            None
        };

        // WP22 T1 / RM-027 round 4 — inside the org lock: validate and commit
        // the authoritative org placement, then publish the directory. Values
        // are read back out of the patched document rather than from the
        // override params, so the pack's own wiring (workers → the pack's
        // front desk) is captured when the admin supplied no override. An
        // authority refusal fails the create and leaves nothing behind.
        {
            let org_entry = {
                let agent_tbl = parsed_agent.get("agent").and_then(|v| v.as_table());
                let field = |k: &str| {
                    agent_tbl
                        .and_then(|t| t.get(k))
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                duduclaw_core::OrgEntry::new(field("reports_to"), field("department"))
            };
            // Prepare org authority before entering users.db, exactly as the
            // blank-avatar writer does; no second SQLite writer inside the tx.
            let result = if let (Some(db), Some(member), Some(actor)) =
                (avatar_db.as_ref(), avatar.member(), avatar.actor()) {
                let prior_org = duduclaw_core::org_store::load(&self.home_dir).get(&name).cloned();
                let mut prepared_org = false;
                let mut published = false;
                let result = duduclaw_core::org_store::upsert(&org, &name, org_entry.clone())
                    .map_err(|e| e.to_string()).and_then(|_| {
                        prepared_org = true;
                        let admission = own_admission.as_ref().or(batch_admission)
                            .ok_or("Avatar seat admission unavailable")?;
                        db.publish_new_avatar_under_as(actor, member, &name, admission, seat_cap, |conn| {
                            let result = staged.publish_in_users_transaction(&org, org_entry, conn);
                            published = match &publication {
                                Some(proof) => proof.published(),
                                None => match std::fs::symlink_metadata(&agent_dir) {
                                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
                                    _ => true,
                                },
                            };
                            result.map(|_| ()).map_err(|e| e.to_string())
                        })
                    });
                result.map_err(|mut error| {
                    if published || prepared_org {
                        let cleanup = if published { std::fs::remove_dir_all(&agent_dir) } else { Ok(()) };
                        let authority = match prior_org {
                            Some(entry) => duduclaw_core::org_store::upsert(&org, &name, entry).map(|_| ()),
                            None => duduclaw_core::org_store::remove(&org, &name).map(|_| ()),
                        };
                        if cleanup.as_ref().is_err_and(|e| e.kind() != std::io::ErrorKind::NotFound) || authority.is_err() {
                            error.push_str("；AI 建立失敗，清理未完成，請管理員檢查 AI 資料。未取得名額豁免。");
                        }
                    }
                    error
                })
            } else {
                staged.publish(&org, org_entry).map(|_| ()).map_err(|e| e.to_string())
            };
            if let Err(e) = result {
                warn!(agent = %name, error = %e, "templates.create_agent refused: org authority not committed");
                if !avatar.is_signin() && !disabled_skills.is_empty() {
                    let _ = duduclaw_agent::skill_state::set_disabled(&self.home_dir, &name, disabled_skills, false);
                }
                // RM-027 round 5: the create did not happen — put back the
                // main it demoted (still under the org lock).
                let restore = match demotion {
                    Some(d) => self.restore_demoted_main(d).await.err(),
                    None => None,
                };
                drop(signin_skills.take());
                drop(org);
                drop(own_admission);
                return WsFrame::error_response(
                    "",
                    &super::agents_create::create_failed_text(&e.to_string(), restore.as_deref()),
                );
            }
        }
        if let Some(rollback) = &mut signin_skills { rollback.committed = true; }
        drop(publication);
        drop(org);
        // Verify the effective artifact before either the own or batch
        // admission can be released.
        // Avatar recount already ran inside the users transaction, while both
        // locks were held. Do not add a fallible step after that commit.
        if avatar.member().is_none() {
            if let Some(admission) = own_admission.as_ref().or(batch_admission) {
                if let Err(error) = admission.recount(self.effective_agent_cap().await) {
                    return WsFrame::error_response("", &error);
                }
            }
        }
        drop(own_admission);

        // Same protections as agents.create: file-guard hook + registry rescan.
        alignment.audit(&self.home_dir, &name, "templates.create_agent",
            caller.map(|ctx| ctx.user_id.as_str()).unwrap_or("system:google_signin"));

        let bin = crate::agent_hook_installer::resolve_duduclaw_bin();
        if let Err(e) =
            crate::agent_hook_installer::ensure_agent_hook_settings(&agent_dir, &bin).await
        {
            warn!(agent = %name, error = %e, "Failed to install agent-file-guard hook on templates.create_agent");
        }
        {
            let mut reg = self.registry.write().await;
            if let Err(e) = reg.scan().await {
                warn!(name, error = %e, "agent created but registry rescan failed");
            }
        }

        // WP2.1 方案 A:產業包若隨附此角色的行為題庫,一併落到
        // `<home>/evals/<name>/`(預設 eval_suites_root),讓包裝出的
        // AI 員工第一天就有 AEE case 維度與 E1 斷言重放的真實基線。
        // 非致命:沒有題庫(免費角色/自建)或安裝失敗都不影響建立。
        let eval_suite_cases = if premium && install_evals && pt::is_safe_slug(&assembled.name) { match pt::install_eval_suite(
            &premium_dir,
            &assembled.name,
            &name,
            &self.home_dir,
        )
        .await
        {
            Ok(n) => n,
            Err(e) => {
                warn!(agent = %name, error = %e, "eval suite install failed on templates.create_agent");
                None
            }
        } } else { None };
        if let Some(n) = eval_suite_cases {
            info!(agent = %name, cases = n, "installed premium eval suite");
        }

        info!(name, role_id, "Agent created from premium template");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
                "runtime_provider_auto_aligned": alignment.aligned_provider().map(|p|p.as_str().to_owned()),
                "runtime_provider_align_skipped": alignment.skipped_reason(),
                "warning": extras_warning,
                "agent": {
                    "name": name,
                    "role": final_role,
                    "role_id": role_id,
                },
                "eval_suite_cases": eval_suite_cases,
            }),
        )
    }
}

/// Roll back skill switches on every onboarding failure, including an early
/// write error or a users transaction failure after the directory was published.
struct SigninSkillRollback {
    home: PathBuf,
    name: String,
    added: Vec<String>,
    written: bool,
    committed: bool,
}

impl Drop for SigninSkillRollback {
    fn drop(&mut self) {
        if self.written && !self.committed && !self.added.is_empty() {
            if let Err(error) = duduclaw_agent::skill_state::set_disabled(&self.home, &self.name, &self.added, false) {
                warn!(agent = %self.name, error = %error, "signin avatar skill rollback failed");
            }
        }
    }
}

/// Same final-document preparation used by the licensed template writer.
pub(super) fn prepare_template_runtime(home: &Path, original: &str) -> Result<(String,crate::runtime_config::ModelRuntimeAlignment),String> {
    let mut table:toml::Table=original.parse().map_err(|e|format!("Invalid agent.toml: {e}"))?;
    let evolution = table.entry("evolution").or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut().ok_or("Invalid [evolution] section")?;
    for key in ["enabled", "gvu_enabled"] {
        evolution.entry(key).or_insert(true.into());
        if evolution[key].as_bool().is_none() { return Err("Invalid evolution switch".into()); }
    }
    let alignment=crate::runtime_config::align_model_runtime(&mut table)?;
    let rendered=super::config_commit::render_preserving(original,&table)?;
    let final_table:toml::Table=rendered.parse().map_err(|e|format!("Invalid agent.toml: {e}"))?;
    crate::product_policy::load(home).and_then(|policy|crate::product_policy::validate_table(policy,&final_table))?;
    Ok((rendered,alignment))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn templates_create_agent_checks_final_caller_text_against_absent_baseline() {
        let home = tempfile::tempdir().unwrap();
        let assets = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(home.path().into()).await;
        handler.set_agent_cap_fixture(Some(0)).await;
        let assembled = crate::premium_templates::AssembledRole {
            role_id: "owned-role".into(), kind: crate::premium_templates::RoleKind::Worker,
            name: "template".into(), display_name: "Template".into(), trigger: "@Template".into(),
            reports_to: String::new(), summary: "Owned fixture".into(), soul_md: "Helpful".into(),
            contract_toml: String::new(), extras_dir: None,
            agent_toml: include_str!("../../../../templates/evaluator/agent.toml").into(),
        };
        let admin = UserContext::admin_fallback();
        let bad = format!("{}\n[odoo]\nact_as_requester = true\n", assembled.agent_toml);
        let response = handler.create_assembled_template_agent(
            &json!({"name":"unsafe","agent_toml":bad}), &admin, assets.path(), "owned-role", &assembled,
        ).await;
        assert!(matches!(response, WsFrame::Response { ok: false, error: Some(ref e), .. }
            if e.as_str().is_some_and(|s| s.contains("這位 AI 員工的 Odoo 權限只能在該 AI 員工的編輯頁確認風險後變更。"))));
        assert!(!home.path().join("agents/unsafe").exists());
        assert!(handler.registry.read().await.get("unsafe").is_none());
        // Team/template batch apply holds one admission and reaches the same
        // final-text writer with assembled content rather than editor params.
        let admission = match handler.admit_agent_seats(1, None).await {
            Ok(value) => value,
            Err(_) => panic!("owned fixture must admit one employee"),
        };
        let mut unsafe_role = assembled.clone();
        unsafe_role.agent_toml = bad;
        let response = handler.create_template_with_skills(
            &json!({"name":"unsafe-batch"}), &admin, assets.path(), "owned-role", &unsafe_role,
            Some(&std::collections::BTreeMap::new()), false, false, &[], Some(&admission),
        ).await;
        assert!(matches!(response, WsFrame::Response { ok: false, error: Some(ref e), .. }
            if e.as_str().is_some_and(|s| s.contains("這位 AI 員工的 Odoo 權限只能在該 AI 員工的編輯頁確認風險後變更。"))));
        assert!(!home.path().join("agents/unsafe-batch").exists());
        drop(admission);
        for (index, fields) in ["", "permission_override='inherit'", "permission_override='engine'",
            "permission_override='read_only'", "permission_override='unknown'"].into_iter().enumerate() {
            let name = format!("safe-{index}");
            let text = format!("{}\n[odoo]\n{fields}\n", assembled.agent_toml);
            let response = handler.create_assembled_template_agent(
                &json!({"name":name,"agent_toml":text}), &admin, assets.path(), "owned-role", &assembled,
            ).await;
            assert!(matches!(response, WsFrame::Response { ok: true, .. }), "{response:?}");
            let published = std::fs::read_to_string(home.path().join("agents").join(&name).join("agent.toml")).unwrap();
            super::super::config_commit::refuse_odoo_agent_widening_text_edit("", &published).unwrap();
            assert!(handler.registry.read().await.get(&name).is_some());
        }
    }

    #[test]
    fn templates_create_evolution_defaults_on_and_preserves_explicit_false() {
        let home = tempfile::tempdir().unwrap();
        for (settings, enabled, gvu) in [
            ("", true, true),
            ("[evolution]\n", true, true),
            ("[evolution]\nenabled = false\n", false, true),
            ("[evolution]\ngvu_enabled = false\n", true, false),
        ] {
            let raw = format!("[runtime]\nprovider = 'claude'\n[model]\npreferred = 'claude-sonnet-4-6'\n{settings}");
            let (rendered, _) = prepare_template_runtime(home.path(), &raw).unwrap();
            let config: toml::Table = rendered.parse().unwrap();
            assert_eq!(config["evolution"]["enabled"].as_bool(), Some(enabled));
            assert_eq!(config["evolution"]["gvu_enabled"].as_bool(), Some(gvu));
        }
    }
}
