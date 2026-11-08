//! H12d: CLI side of protected `skill_from_recording`.
//!
//! The recording is the caller's own finished protected recording: the job
//! store (HX6 seam) proves ownership and completion and supplies the artifact
//! manifest; the artifacts are read through root's work broker from the
//! employee's own `employee-work` root at the helper's layout. Nothing is read
//! from (or written to) the host `recordings/` directory. Until HX6 installs a
//! recording job store the tool is not eligible (hidden) and this refuses.
use std::path::Path;
use serde_json::Value;

const REFUSED: &str = "無法確認錄製的擁有者、完成狀態或來源檔案，已停止蒸餾。";
/// Shown when the protected recording job store is not installed yet.
pub(crate) const PENDING_HX6: &str = "受保護錄製的工作來源尚未就緒（HX6），目前無法從錄製蒸餾技能。";

/// The recording metadata and E's summarizer input for this isolated call.
/// `agent` must be the call's capability-bound employee.
pub(crate) async fn recording_input(home: &Path, args: &Value, agent: &str)
    -> Result<(crate::mcp_recording::RecordingMeta, Vec<u8>), &'static str> {
    let current = crate::mcp_isolation::current().ok_or(REFUSED)?;
    if current.agent != agent || home != current.home { return Err(REFUSED); }
    if duduclaw_mcp_boundary::tool_policy::recording_jobs().is_none() { return Err(PENDING_HX6); }
    let call = crate::mcp_isolation::authorize_gateway_handler(home, "skill_from_recording", args).await
        .map_err(|_| REFUSED)?;
    if call.context().record() != current.context.record() { return Err(REFUSED); }
    let roots = current.roots.clone();
    // A broker round trip can take seconds: keep it off the async workers.
    let (manifest, bytes) = tokio::task::spawn_blocking(move ||
        duduclaw_gateway::isolation_skill_jobs::recording_summary_input(&call, &roots)).await
        .map_err(|_| REFUSED)?
        .map_err(|refusal| refusal.broker().map_or(REFUSED, |error| error.user_message()))?;
    // Protected recordings are headless browser recordings (HX6 may add more).
    let meta = crate::mcp_recording::RecordingMeta { id: manifest.id, kind: "browser".into(),
        name: "recording".into(), url: None, agent: agent.to_owned(), pid: None, started_at: String::new() };
    Ok((meta, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn h12d_unprotected_calls_never_take_the_brokered_path() {
        let home = tempfile::tempdir().unwrap();
        // Outside an isolated call there is no capability to authorize against.
        assert_eq!(recording_input(home.path(), &json!({"id":"ab".repeat(16)}), "main").await.err(), Some(REFUSED));
    }

    #[test]
    fn h12bcd_protected_arguments_are_exact_and_self_only() {
        use crate::mcp_isolation::check_tool_arguments;
        let hub = json!({"hub":"clawhub","skill_name":"notes"});
        assert!(check_tool_arguments("skill_hub_install", &hub).is_ok());
        for extra in [("scope", json!("global")), ("scope", json!("other-agent")), ("owner", json!("someone")),
            ("agent_id", json!("main")), ("target_agent", json!("other")), ("employee_id", json!("x"))] {
            let mut args = hub.clone(); args[extra.0] = extra.1;
            assert!(check_tool_arguments("skill_hub_install", &args).is_err(), "{args}");
        }
        assert!(check_tool_arguments("skill_hub_install", &json!({"hub":"clawhub"})).is_err());
        for args in [json!({}), json!({"dry_run":false,"lookback_days":3}), json!({"agent_id":"main"})] {
            assert!(check_tool_arguments("skill_synthesis_run", &args).is_ok(), "{args}");
        }
        for args in [json!({"dry_run":"false"}), json!({"lookback_days":0}), json!({"target_agent":"other"}),
            json!({"agent_id":"../x"}), json!({"run_id":"x"})] {
            assert!(check_tool_arguments("skill_synthesis_run", &args).is_err(), "{args}");
        }
        assert!(check_tool_arguments("skill_from_recording", &json!({"id":"ab".repeat(16),"name":"報表"})).is_ok());
        // Protected ids are job handles; legacy host recording ids never apply.
        for args in [json!({"id":"rec-20261108000000-abcdef"}), json!({"id":"ab".repeat(16),"path":"recordings/x"}),
            json!({"id":"ab".repeat(16),"workspace":"employee-work"}), json!({})] {
            assert!(check_tool_arguments("skill_from_recording", &args).is_err(), "{args}");
        }
    }

    #[tokio::test]
    async fn h12bcd_unprotected_handlers_keep_their_legacy_paths() {
        let home = tempfile::tempdir().unwrap();
        // No isolated call: the original validation and messages, unchanged.
        let result = crate::mcp::handle_skill_hub_install(&json!({}), home.path(), "dudu", true).await;
        assert_eq!(result["isError"], true);
        assert!(result.to_string().contains("hub and skill_name are required"), "{result}");
        let result = crate::mcp_recording_distill::handle_skill_from_recording(&json!({"id":"ab".repeat(16)}),
            home.path(), "main").await;
        assert!(result.to_string().contains("需要有效的錄製"), "legacy id rules apply outside isolation: {result}");
    }
}
