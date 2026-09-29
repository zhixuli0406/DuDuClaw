//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Premium team templates ─────────────────────────────────────────────
    // Dashboard onboarding flow: the admin stages an industry (backend
    // prepares the department templates, creates NOTHING), then creates each
    // agent one by one — picking a role, reviewing/editing SOUL.md (and
    // optionally CONTRACT.toml / agent.toml) in a text editor. The
    // cross-industry CEO kit is offered as the suggested first agent.

    /// Gateway-side premium gate. Fail-closed: license runtime not booted,
    /// locked tier, or any error all resolve to *locked*.
    pub(crate) async fn premium_templates_unlocked(&self) -> bool {
        match crate::license_runtime::global() {
            Some(rt) => rt.check_feature("premium_templates").await,
            None => false,
        }
    }

    pub(crate) fn team_staging_path(&self) -> PathBuf {
        self.home_dir.join("team_staging.json")
    }

    /// The industry the admin staged during onboarding, if any.
    pub(crate) async fn read_staged_industry(&self) -> Option<String> {
        let raw = tokio::fs::read_to_string(self.team_staging_path())
            .await
            .ok()?;
        let v: Value = serde_json::from_str(&raw).ok()?;
        let industry = v.get("industry")?.as_str()?;
        if industry.is_empty() {
            None
        } else {
            Some(industry.to_string())
        }
    }

    /// Resolve the premium templates dir behind the license gate, or the
    /// standard locked/absent error frame.
    pub(crate) async fn premium_dir_unlocked(&self) -> Result<PathBuf, WsFrame> {
        if !self.premium_templates_unlocked().await {
            return Err(WsFrame::error_response(
                "",
                "Premium 產業板模未解鎖：需要 Pro 以上授權",
            ));
        }
        crate::premium_templates::find_premium_templates_dir()
            .ok_or_else(|| WsFrame::error_response("", "此安裝未附 Premium 板模資源"))
    }

    /// Build the roster payload for a (possibly absent) staged team: CEO kit
    /// first, then front desk + workers, with `created` resolved against the
    /// live registry so the UI can show which roles are already filled.
    pub(crate) async fn team_roster_json(
        &self,
        premium_dir: &Path,
        manifest: Option<&crate::premium_templates::TeamManifest>,
    ) -> Value {
        use crate::premium_templates as pt;
        let existing: std::collections::HashSet<String> = self
            .registry
            .read()
            .await
            .list()
            .iter()
            .map(|a| a.config.agent.name.clone())
            .collect();
        let mut roles = Vec::new();
        if let Ok(ceo) = pt::assemble_ceo(premium_dir) {
            roles.push(json!({
                "role_id": pt::CEO_ROLE_ID,
                "kind": "ceo",
                "name": ceo.name,
                "display_name": ceo.display_name,
                "summary": ceo.summary,
                "created": existing.contains(&ceo.name),
                "overlay_count": 0,
            }));
        }
        if let Some(m) = manifest {
            roles.push(json!({
                "role_id": pt::FRONT_DESK_ROLE_ID,
                "kind": "front_desk",
                "name": m.front_desk.name,
                "display_name": m.front_desk.display_name,
                "summary": m.front_desk.summary,
                "created": existing.contains(&m.front_desk.name),
                "overlay_count": 0,
            }));
            for w in &m.workers {
                roles.push(json!({
                    "role_id": w.name,
                    "kind": "worker",
                    "kit": w.kit,
                    "name": w.name,
                    "display_name": w.display_name,
                    "summary": w.summary,
                    "created": existing.contains(&w.name),
                    "overlay_count": w.overlay.len(),
                }));
            }
        }
        json!({
            "industry": manifest.map(|m| m.industry.clone()),
            "label": manifest.map(|m| m.label.clone()),
            "roles": roles,
            "humans": manifest
                .map(|m| m.humans.iter()
                    .map(|h| json!({ "title": h.title, "summary": h.summary }))
                    .collect::<Vec<_>>())
                .unwrap_or_default(),
            "excluded": manifest
                .map(|m| m.excluded.iter()
                    .map(|e| json!({ "kit": e.kit, "reason": e.reason }))
                    .collect::<Vec<_>>())
                .unwrap_or_default(),
        })
    }
}
