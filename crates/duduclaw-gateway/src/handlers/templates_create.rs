//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// `templates.create_agent` — create ONE agent from a (possibly edited)
    /// role template. Admin-edited SOUL.md is taken as-is; edited
    /// CONTRACT.toml / agent.toml must still parse (fail-closed: an invalid
    /// document is rejected, nothing is written). `[agent].name` is always
    /// forced to the final agent name so the directory and config agree.
    pub(crate) async fn handle_templates_create_agent(&self, params: Value) -> WsFrame {
        use crate::premium_templates as pt;
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
        let parsed_agent: toml::Table = match agent_toml.parse() {
            Ok(t) => t,
            Err(e) => {
                return WsFrame::error_response("", &format!("agent.toml 格式錯誤，未寫入：{e}"));
            }
        };
        let final_role = parsed_agent
            .get("agent")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("role"))
            .and_then(|v| v.as_str())
            .unwrap_or("specialist")
            .to_string();

        // Cloud-tier agent cap (self-host is never capped — Apache 2.0).
        let agent_count = self.registry.read().await.list().len();
        if let Some(msg) = self.tier_limit_message("agent", agent_count).await {
            return WsFrame::error_response("", &msg);
        }

        let agents_dir = self.registry.read().await.agents_dir().to_path_buf();
        let agent_dir = agents_dir.join(&name);

        // Atomic directory claim: `create_dir` (not `create_dir_all`) fails on
        // AlreadyExists, so two concurrent creates can't both win the name.
        if let Err(e) = tokio::fs::create_dir_all(&agents_dir).await {
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }
        if let Err(e) = tokio::fs::create_dir(&agent_dir).await {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                return WsFrame::error_response("", &format!("Agent '{name}' already exists"));
            }
            return WsFrame::error_response("", &format!("Failed to create directory: {e}"));
        }

        // Creating a new main (CEO / front desk) demotes the current one —
        // only AFTER the name claim succeeded, so a doomed create can no
        // longer demote the live main as a side effect.
        if final_role == "main" {
            if let Err(e) = self.demote_current_main(&name).await {
                let _ = tokio::fs::remove_dir_all(&agent_dir).await;
                return WsFrame::error_response("", &e);
            }
        }

        // Write SOUL.md / CONTRACT.toml / extras first; the atomic agent.toml
        // rename comes LAST as the registry-visible commit point. Any failure
        // before that point removes the whole directory — no half-created
        // agent (one missing its reviewed CONTRACT) can ever be scanned in.
        let write_result: Result<Option<String>, String> = async {
            tokio::fs::create_dir_all(agent_dir.join("SKILLS"))
                .await
                .map_err(|e| format!("Failed to create directory: {e}"))?;
            Self::seed_builtin_skills(&agent_dir.join("SKILLS"));
            tokio::fs::write(agent_dir.join("SOUL.md"), &soul_md)
                .await
                .map_err(|e| format!("Failed to write SOUL.md: {e}"))?;
            tokio::fs::write(agent_dir.join("CONTRACT.toml"), &contract_toml)
                .await
                .map_err(|e| format!("Failed to write CONTRACT.toml: {e}"))?;

            // Front desk carries the industry pack's knowledge extras
            // (FAQ.json + wiki/). Best-effort: a copy failure is surfaced as a
            // warning but doesn't abort the create.
            let mut extras_warning: Option<String> = None;
            if let Some(src) = &assembled.extras_dir {
                if let Err(e) = copy_template_extras(src, &agent_dir).await {
                    warn!(agent = %name, error = %e, "template extras copy failed");
                    extras_warning = Some("知識檔（FAQ/wiki）複製失敗，可稍後手動補上".to_string());
                }
            }

            let agent_toml_path = agent_dir.join("agent.toml");
            let agent_toml_tmp = agent_toml_path.with_extension("toml.tmp");
            tokio::fs::write(&agent_toml_tmp, &agent_toml)
                .await
                .map_err(|e| format!("Failed to write agent.toml.tmp: {e}"))?;
            tokio::fs::rename(&agent_toml_tmp, &agent_toml_path)
                .await
                .map_err(|e| format!("Failed to commit agent.toml: {e}"))?;
            Ok(extras_warning)
        }
        .await;
        let extras_warning = match write_result {
            Ok(w) => w,
            Err(e) => {
                let _ = tokio::fs::remove_dir_all(&agent_dir).await;
                return WsFrame::error_response("", &e);
            }
        };

        // WP22 T1 — the staged team member is committed; record its
        // authoritative org placement. Values are read back out of the patched
        // document rather than from the override params, so the pack's own
        // wiring (workers → the pack's front desk) is captured when the admin
        // supplied no override. After the commit, so a rolled-back create
        // never leaves a record behind for the id.
        {
            let agent_tbl = parsed_agent.get("agent").and_then(|v| v.as_table());
            let field = |k: &str| {
                agent_tbl
                    .and_then(|t| t.get(k))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
            };
            if let Err(e) = duduclaw_core::org_store::upsert(
                &self.home_dir,
                &name,
                duduclaw_core::OrgEntry::new(field("reports_to"), field("department")),
            ) {
                warn!(agent = %name, error = %e, "org.toml upsert failed on templates.create_agent");
            }
        }

        // Same protections as agents.create: file-guard hook + registry rescan.
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
        let eval_suite_cases = match pt::install_eval_suite(
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
        };
        if let Some(n) = eval_suite_cases {
            info!(agent = %name, cases = n, "installed premium eval suite");
        }

        info!(name, role_id, "Agent created from premium template");
        WsFrame::ok_response(
            "",
            json!({
                "success": true,
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
