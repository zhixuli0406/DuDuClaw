//! Admin RPCs for durable computer-use workspaces (P2-C design §8.6):
//! `computer_workspaces.{list,fence,revoke,regrant,renew,rebind_runner,delete}`.
//! Admin only; every change writes a registry event; answers never carry
//! file content. Fence / revoke / rebind / delete return after the barrier
//! (no further AI click or keystroke on the session that held it).
use super::*;
use crate::computer_use_sessions::workspace_admin::admin_sessions;

impl MethodHandler {
    pub(crate) async fn handle_computer_workspaces(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        if let Err(e) = crate::approval::require_current_dashboard_role_in_home(
            &self.home_dir,
            ctx,
            UserRole::Admin,
        ) {
            return WsFrame::error_response("", &e);
        }
        let sessions = admin_sessions(&self.home_dir);
        let operator = if ctx.email.is_empty() {
            ctx.user_id.as_str()
        } else {
            ctx.email.as_str()
        };
        let id = params["workspace_id"].as_str().unwrap_or("");
        let result = match method {
            "computer_workspaces.list" => {
                sessions
                    .admin_workspace_list(params["owner"].as_str())
                    .await
            }
            "computer_workspaces.fence" => {
                sessions
                    .admin_workspace_fence(
                        id,
                        operator,
                        params["reason"].as_str().unwrap_or("operator_fence"),
                    )
                    .await
            }
            "computer_workspaces.revoke" => sessions.admin_workspace_revoke(id, operator).await,
            "computer_workspaces.regrant" => sessions.admin_workspace_regrant(id, operator).await,
            "computer_workspaces.renew" => sessions.admin_workspace_renew(id, operator).await,
            "computer_workspaces.rebind_runner" => {
                sessions.admin_workspace_rebind_runner(id, operator).await
            }
            "computer_workspaces.delete" => sessions.admin_workspace_delete(id, operator).await,
            _ => return WsFrame::error_response("", "unknown computer_workspaces method"),
        };
        match result {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &e.message),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn succeeded(f: &WsFrame) -> bool {
        matches!(f, WsFrame::Response { ok: true, .. })
    }

    #[tokio::test]
    async fn computer_workspace_rpcs_are_admin_only_and_carry_no_content() {
        let dir = tempfile::tempdir().unwrap();
        let handler = MethodHandler::new(dir.path().to_path_buf()).await;
        let admin = UserContext::admin_fallback();
        let mut employee = admin.clone();
        employee.role = UserRole::Employee;
        let store = crate::computer_workspaces::WorkspaceStore::open(dir.path()).unwrap();
        let id = store.create("alice", "local-docker:x", 1, 3).unwrap();
        crate::computer_workspaces::paths::create_workspace_dirs(dir.path(), &id).unwrap();
        store
            .transition(
                &id,
                &[crate::computer_workspaces::WorkspaceState::Creating],
                crate::computer_workspaces::WorkspaceState::Ready,
                "system:t",
                "created",
                None,
            )
            .unwrap();
        let data = dir
            .path()
            .canonicalize()
            .unwrap()
            .join("computer_workspaces")
            .join(&id)
            .join("data");
        std::fs::write(data.join("secret.txt"), "TOP-SECRET-CONTENT").unwrap();
        for method in [
            "computer_workspaces.list",
            "computer_workspaces.fence",
            "computer_workspaces.revoke",
            "computer_workspaces.regrant",
            "computer_workspaces.renew",
            "computer_workspaces.rebind_runner",
            "computer_workspaces.delete",
        ] {
            assert!(
                !succeeded(
                    &handler
                        .handle(method, json!({"workspace_id": id}), &employee)
                        .await
                ),
                "{method}"
            );
        }
        let list = handler
            .handle("computer_workspaces.list", json!({}), &admin)
            .await;
        assert!(succeeded(&list));
        let text = serde_json::to_string(&list).unwrap();
        assert!(text.contains(&id) && !text.contains("TOP-SECRET") && !text.contains("secret.txt"));
        assert!(succeeded(
            &handler
                .handle(
                    "computer_workspaces.fence",
                    json!({"workspace_id": id}),
                    &admin
                )
                .await
        ));
        assert!(succeeded(
            &handler
                .handle(
                    "computer_workspaces.revoke",
                    json!({"workspace_id": id}),
                    &admin
                )
                .await
        ));
        assert!(succeeded(
            &handler
                .handle(
                    "computer_workspaces.regrant",
                    json!({"workspace_id": id}),
                    &admin
                )
                .await
        ));
        assert!(succeeded(
            &handler
                .handle(
                    "computer_workspaces.delete",
                    json!({"workspace_id": id}),
                    &admin
                )
                .await
        ));
        assert!(!data.exists());
        let row = store.get(&id).unwrap().unwrap();
        assert_eq!(
            row.state,
            crate::computer_workspaces::WorkspaceState::Deleted
        );
    }
}
