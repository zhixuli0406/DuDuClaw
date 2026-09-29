//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// `templates.industries` — list industries that ship a team manifest.
    /// Never errors on locked/absent: the UI needs the upsell flags.
    pub(crate) async fn handle_templates_industries(&self) -> WsFrame {
        use crate::premium_templates as pt;
        let unlocked = self.premium_templates_unlocked().await;
        let dir = pt::find_premium_templates_dir();
        let staged = self.read_staged_industry().await;
        let (industries, ceo, present) = match &dir {
            Some(d) => {
                let list = pt::list_team_industries(d);
                let present = !list.is_empty() || pt::ceo_available(d);
                if unlocked {
                    let ceo = pt::ceo_available(d);
                    (list, ceo, present)
                } else {
                    (Vec::new(), false, present)
                }
            }
            None => (Vec::new(), false, false),
        };
        WsFrame::ok_response(
            "",
            json!({
                "unlocked": unlocked,
                "present_but_locked": !unlocked && present,
                "staged": staged,
                "ceo_available": ceo,
                "industries": industries.iter().map(|t| json!({
                    "industry": t.industry,
                    "label": t.label,
                    "pack": t.pack,
                    "worker_count": t.worker_count,
                })).collect::<Vec<_>>(),
            }),
        )
    }

    /// `templates.stage` — record the chosen industry and return its roster.
    /// Prepares templates only; creates NO agents (the admin creates each one
    /// explicitly afterwards).
    pub(crate) async fn handle_templates_stage(&self, params: Value) -> WsFrame {
        use crate::premium_templates as pt;
        let industry = params
            .get("industry")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let premium_dir = match self.premium_dir_unlocked().await {
            Ok(d) => d,
            Err(frame) => return frame,
        };
        let manifest = match pt::load_team_manifest(&premium_dir, industry) {
            Ok(m) => m,
            Err(e) => {
                warn!(industry, error = %e, "templates.stage: manifest load failed");
                return WsFrame::error_response(
                    "",
                    &format!("無法載入產業板模：{}", scrub_premium_path(&e, &premium_dir)),
                );
            }
        };
        let staging = serde_json::to_string_pretty(&json!({
            "industry": manifest.industry,
            "staged_at": Utc::now().to_rfc3339(),
        }))
        .unwrap_or_default();
        // Atomic write — same temp+rename discipline as agent.toml.
        let path = self.team_staging_path();
        let tmp = path.with_extension("json.tmp");
        if let Err(e) = tokio::fs::write(&tmp, &staging).await {
            return WsFrame::error_response("", &format!("Failed to write staging: {e}"));
        }
        if let Err(e) = tokio::fs::rename(&tmp, &path).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return WsFrame::error_response("", &format!("Failed to commit staging: {e}"));
        }
        info!(industry, "team templates staged");
        let roster = self.team_roster_json(&premium_dir, Some(&manifest)).await;
        WsFrame::ok_response("", json!({ "success": true, "roster": roster }))
    }

    /// `templates.roster` — the staged (or explicitly named) team's roster.
    /// With nothing staged, still returns the CEO kit so the create dialog
    /// can offer it as the first-agent template.
    pub(crate) async fn handle_templates_roster(&self, params: Value) -> WsFrame {
        use crate::premium_templates as pt;
        let premium_dir = match self.premium_dir_unlocked().await {
            Ok(d) => d,
            Err(frame) => return frame,
        };
        let industry = match params.get("industry").and_then(|v| v.as_str()) {
            Some(i) if !i.is_empty() => Some(i.to_string()),
            _ => self.read_staged_industry().await,
        };
        let manifest = match &industry {
            Some(ind) => match pt::load_team_manifest(&premium_dir, ind) {
                Ok(m) => Some(m),
                Err(e) => {
                    warn!(industry = %ind, error = %e, "templates.roster: manifest load failed");
                    return WsFrame::error_response(
                        "",
                        &format!("無法載入產業板模：{}", scrub_premium_path(&e, &premium_dir)),
                    );
                }
            },
            None => None,
        };
        let roster = self.team_roster_json(&premium_dir, manifest.as_ref()).await;
        WsFrame::ok_response("", roster)
    }

    /// Assemble the deploy-ready default files for a role (shared by
    /// `templates.role` and `templates.create_agent`).
    pub(crate) async fn assemble_template_role(
        &self,
        premium_dir: &Path,
        params: &Value,
        role_id: &str,
    ) -> Result<crate::premium_templates::AssembledRole, String> {
        use crate::premium_templates as pt;
        if role_id == pt::CEO_ROLE_ID {
            return pt::assemble_ceo(premium_dir);
        }
        let industry = match params.get("industry").and_then(|v| v.as_str()) {
            Some(i) if !i.is_empty() => i.to_string(),
            _ => self
                .read_staged_industry()
                .await
                .ok_or_else(|| "尚未選擇產業（請先備妥產業板模）".to_string())?,
        };
        let manifest = pt::load_team_manifest(premium_dir, &industry)?;
        pt::assemble_role(premium_dir, &manifest, role_id)
    }

    /// `templates.role` — the editable file set for one role: SOUL.md prompt
    /// (text-editor ready), CONTRACT.toml with overlay applied, agent.toml
    /// with identity pre-wired.
    pub(crate) async fn handle_templates_role(&self, params: Value) -> WsFrame {
        let role_id = params.get("role_id").and_then(|v| v.as_str()).unwrap_or("");
        if role_id.is_empty() {
            return WsFrame::error_response("", "role_id is required");
        }
        let premium_dir = match self.premium_dir_unlocked().await {
            Ok(d) => d,
            Err(frame) => return frame,
        };
        match self
            .assemble_template_role(&premium_dir, &params, role_id)
            .await
        {
            Ok(r) => WsFrame::ok_response(
                "",
                json!({
                    "role_id": r.role_id,
                    "kind": r.kind.as_str(),
                    "name": r.name,
                    "display_name": r.display_name,
                    "trigger": r.trigger,
                    "reports_to": r.reports_to,
                    "summary": r.summary,
                    "soul_md": r.soul_md,
                    "contract_toml": r.contract_toml,
                    "agent_toml": r.agent_toml,
                    "has_extras": r.extras_dir.is_some(),
                }),
            ),
            Err(e) => {
                warn!(role_id, error = %e, "templates.role: assembly failed");
                WsFrame::error_response(
                    "",
                    &format!("無法組裝角色板模：{}", scrub_premium_path(&e, &premium_dir)),
                )
            }
        }
    }
}
