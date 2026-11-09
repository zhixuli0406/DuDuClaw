//! H12d / SFR: CLI side of protected `skill_from_recording`.
//!
//! The recording is the caller's own finished protected recording: HX6's job
//! store (installed at gateway boot as H12d's `RecordingJobs` seam) proves
//! ownership (employee + principal + binding of this call) and completion and
//! supplies the artifact digests; the artifacts are read through root's work
//! broker from the employee's own `employee-work` root at the helper's layout
//! and come back only as `VerifiedArtifact`s. They are still untrusted content
//! and are parsed only by E ([`summarize`] takes a `VerifiedRecording`). The
//! distilled skill is published only into the caller's own SKILLS after E's
//! scan of the exact bytes. Nothing is read from (or written to) the host
//! `recordings/` directory or the custom-skill draft/approval store.
use std::path::Path;
use duduclaw_gateway::isolation_skill_jobs::{self, VerifiedRecording};
use duduclaw_mcp_boundary::tool_policy::AuthorizedToolCall;
use duduclaw_parse_worker::worker::RecordingSummary;
use serde_json::Value;
use crate::mcp_recording_distill::{distill_with_model, is_valid_skill_slug, rec_error, rec_text, slugify};

const REFUSED: &str = "無法確認錄製的擁有者、完成狀態或來源檔案，已停止蒸餾。";
/// Shown when no protected recording job store is installed (boot incomplete).
pub(crate) const SOURCE_UNAVAILABLE: &str = "受保護錄製的工作來源尚未就緒，目前無法從錄製蒸餾技能。";
const MODIFIED: &str = "錄製檔案在完成後已被修改（artifact_modified），已拒絕蒸餾；請重新錄製。";
const SUMMARY_FAILED: &str = "隔離服務無法整理這份錄製（檔案格式、大小或內容未通過檢查），已停止蒸餾。";

/// The caller's own finished recording, verified, with the call it was read for.
pub(crate) struct ProtectedRecording {
    pub(crate) call: AuthorizedToolCall,
    pub(crate) recording: VerifiedRecording,
}

/// Authorize this isolated call and read its recording. `agent` must be the
/// call's capability-bound employee.
pub(crate) async fn recording_input(home: &Path, args: &Value, agent: &str) -> Result<ProtectedRecording, &'static str> {
    let current = crate::mcp_isolation::current().ok_or(REFUSED)?;
    if current.agent != agent || home != current.home { return Err(REFUSED); }
    if duduclaw_mcp_boundary::tool_policy::recording_jobs().is_none() { return Err(SOURCE_UNAVAILABLE); }
    let call = crate::mcp_isolation::authorize_gateway_handler(home, "skill_from_recording", args).await
        .map_err(|_| REFUSED)?;
    if call.context().record() != current.context.record() { return Err(REFUSED); }
    let (reader, roots) = (call.clone(), current.roots.clone());
    // A broker round trip can take seconds: keep it off the async workers.
    let (_, recording) = tokio::task::spawn_blocking(move ||
        isolation_skill_jobs::recording_artifacts(&reader, &roots)).await
        .map_err(|_| REFUSED)?
        .map_err(|refusal| match (refusal.artifact(), refusal.broker()) {
            (Some(_), _) => MODIFIED,
            (None, Some(error)) => error.user_message(),
            (None, None) => REFUSED,
        })?;
    Ok(ProtectedRecording { call, recording })
}

/// The distill step: E summarizes the verified artifacts. Its input type is
/// the verified recording, so unverified bytes cannot reach it.
pub(crate) async fn summarize(recording: VerifiedRecording, slug: String) -> Result<RecordingSummary, &'static str> {
    tokio::task::spawn_blocking(move || isolation_skill_jobs::summarize_recording(recording, &slug)).await
        .map_err(|_| SUMMARY_FAILED)?.map_err(|_| SUMMARY_FAILED)
}

/// Protected `skill_from_recording` (the id was checked by the caller).
pub(crate) async fn handle_protected(args: &Value, home: &Path, agent: &str) -> Value {
    let input = match recording_input(home, args, agent).await {
        Ok(input) => input,
        Err(message) => return rec_error(message),
    };
    let slug = slugify(args.get("name").and_then(Value::as_str).filter(|name| !name.trim().is_empty())
        .unwrap_or("recording"));
    if !is_valid_skill_slug(&slug) { return rec_error("無效的技能名稱（僅限 a-z、0-9、-、_）。"); }
    let id = input.recording.id().to_owned();
    let summary = match summarize(input.recording, slug.clone()).await {
        Ok(summary) => summary,
        Err(message) => return rec_error(message),
    };
    let env: Vec<String> = summary.env.as_slice().iter().map(|s| s.as_str().to_owned()).collect();
    let skill_md = match distill_with_model(home, agent, summary.prompt.as_str(), &env).await {
        Ok(markdown) => markdown,
        Err(error) => return rec_error(&error),
    };
    let _admission = match crate::mcp::admit_background_identity(home, agent, "skill_from_recording").await {
        Ok(guard) => guard,
        Err(reason) => return rec_error(&reason),
    };
    // E scans these exact bytes right before the no-clobber write into the
    // caller's own SKILLS; a refused scan writes nothing anywhere.
    let risk = match isolation_skill_jobs::publish_owned(home, &input.call, agent, &slug, &skill_md).await {
        Ok(risk) => risk,
        Err(error) => return rec_error(&error),
    };
    rec_text(&format!(
        "✅ 已把你自己的錄製「{id}」蒸餾成技能「{slug}」，並發布到你自己的技能（SKILLS/{slug}.md）。\n\
         - 來源材料：{} 個 API 呼叫、{} 個操作步驟{}（錄製檔案已比對完成時的雜湊，並只在隔離解析服務內解析）\n\
         - 安全掃描：風險 {risk}（隔離服務掃描通過才發布；同名技能不會被覆寫）",
        summary.api_count, summary.action_count,
        if env.is_empty() { String::new() } else { format!("、需要環境變數：{}", env.join(", ")) },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn h12d_unprotected_calls_never_take_the_brokered_path() {
        let home = tempfile::tempdir().unwrap();
        // Outside an isolated call there is no capability to authorize against.
        assert!(matches!(recording_input(home.path(), &json!({"id":"ab".repeat(16)}), "main").await, Err(REFUSED)));
        let result = handle_protected(&json!({"id":"ab".repeat(16)}), home.path(), "main").await;
        assert_eq!(result["isError"], true);
        assert!(!home.path().join("agents").exists() && !home.path().join("skills-drafts").exists());
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
