//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    // ── Department registry handlers ─────────────────────────

    /// Departments referenced anywhere: agent `[agent] department` fields ∪
    /// `shared/{wiki,skills}/departments/*` sub-trees (WP7 derived design).
    pub(crate) async fn agent_department_pairs(&self) -> Vec<(String, String)> {
        let reg = self.registry.read().await;
        reg.list()
            .iter()
            .filter(|a| !a.config.agent.department.is_empty())
            .map(|a| {
                (
                    a.config.agent.department.clone(),
                    a.config.agent.display_name.clone(),
                )
            })
            .collect()
    }

    pub(crate) async fn handle_departments_list(&self) -> WsFrame {
        let pairs = self.agent_department_pairs().await;
        let list = crate::departments::list_departments(&self.home_dir, &pairs);
        WsFrame::ok_response("", json!({ "departments": list }))
    }

    pub(crate) async fn handle_departments_create(&self, params: Value) -> WsFrame {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let pairs = self.agent_department_pairs().await;
        let existing = crate::departments::list_departments(&self.home_dir, &pairs);
        match crate::departments::create_department(&self.home_dir, name, &existing) {
            Ok(()) => {
                info!(department = %name, "department created");
                WsFrame::ok_response("", json!({ "success": true, "name": name }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }

    pub(crate) async fn handle_departments_remove(&self, params: Value) -> WsFrame {
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let force = params
            .get("force")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let pairs = self.agent_department_pairs().await;
        let existing = crate::departments::list_departments(&self.home_dir, &pairs);
        let Some(info) = existing.iter().find(|d| d.name == name) else {
            return WsFrame::error_response("", &format!("部門「{name}」不存在"));
        };
        match crate::departments::remove_department(&self.home_dir, name, info, force) {
            Ok(()) => {
                info!(department = %name, force, "department removed");
                WsFrame::ok_response("", json!({ "success": true }))
            }
            Err(e) => WsFrame::error_response("", &e),
        }
    }
}
