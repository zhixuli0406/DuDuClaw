//! Completion delivery is bound to the gateway's original human turn.
//! No model argument can supply a recipient, bot account, thread or session.
use std::path::Path;

use serde_json::Value;
use super::Job;

#[derive(Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Binding {
    pub reply_channel: Option<String>,
    pub reply_target: Option<Value>,
    pub requester: Option<String>,
    pub session: Option<String>,
}

pub(crate) fn capture(agent: &str, turn: Option<&str>) -> Binding {
    use duduclaw_knowledge::requester::RequesterIdentity;
    let Some(turn) = turn else { return Binding::default(); };
    let requester = match crate::knowledge::turn_requesters::lookup(agent, turn) {
        Some(RequesterIdentity::DashboardUser { user_id }) => user_id,
        Some(RequesterIdentity::ChannelUser { sender, user_id, .. }) => user_id.unwrap_or(sender),
        _ => return Binding::default(),
    };
    Binding {
        reply_channel: crate::turn_context::reply_channel_for(agent, turn),
        reply_target: crate::turn_context::target_for(agent, turn).map(|target| target.notice_descriptor()),
        requester: Some(requester),
        session: crate::turn_context::session_for(agent, turn),
    }
}

fn output_name(value: &Value, key: &str) -> String {
    value[key].as_str().and_then(|s| Path::new(s).file_name()).and_then(|s| s.to_str())
        .map(|s| duduclaw_core::truncate_chars(&s.chars().filter(|c| !c.is_control()).collect::<String>(), 160))
        .unwrap_or_else(|| "逐字稿附件".into())
}

pub(crate) fn format_notice(state: &str, result: Option<&Value>, attached: bool) -> String {
    if state == "failed" {
        return "會議逐字稿未能完成。請檢查錄音是否可以播放，或請管理員確認轉錄服務設定後重試。".into();
    }
    if state == "cancelled" { return "會議逐字稿已取消。".into(); }
    let Some(result) = result.filter(|_| state == "done") else { return "會議逐字稿仍在處理中。".into(); };
    let seconds = result["duration_ms"].as_u64().unwrap_or(0) / 1000;
    let duration = if seconds == 0 { "未提供".into() } else { format!("{} 分 {} 秒", seconds / 60, seconds % 60) };
    let speakers = result["speaker_count"].as_u64().unwrap_or(0);
    let available = if attached { "完整逐字稿已附上，也可在這位員工的附件中取得。" }
        else { "完整逐字稿可在控制台中這位員工的附件下載。" };
    format!("會議逐字稿已完成。\n錄音長度：{duration}；辨識講者：{speakers} 位。\n檔案：{}、{}\n{available}",
        output_name(result, "markdown_path"), output_name(result, "json_path"))
}

/// Persist WebChat completion even if the browser or gateway restarted.
/// The transaction makes retries idempotent and never creates a new session.
async fn persist_webchat(home: &Path, job: &Job, text: &str) -> Result<(), String> {
    let session = job.session.as_deref().ok_or("找不到原對話，請至控制台查看逐字稿。")?;
    let requester = job.requester.as_deref().ok_or("無法確認原對話的使用者。")?;
    if !home.join("sessions.db").is_file() { return Err("原對話已不存在。".into()); }
    let manager = crate::session::SessionManager::new(&home.join("sessions.db"))
        .map_err(|_| "無法讀取原對話。")?;
    let mut conn = manager.acquire().await;
    let tx = conn.transaction().map_err(|_| "無法儲存完成通知。")?;
    let allowed: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=?1 AND agent_id=?2 AND origin_kind='webchat_user' AND owner_user_id=?3)",
        rusqlite::params![session, job.agent, requester], |row| row.get(0),
    ).map_err(|_| "無法確認原對話的使用者。")?;
    if !allowed { return Err("原對話已不存在或無法存取。".into()); }
    tx.execute_batch("CREATE TABLE IF NOT EXISTS meeting_completion_notices(job_id TEXT PRIMARY KEY, session_id TEXT NOT NULL)")
        .map_err(|_| "無法儲存完成通知。")?;
    let inserted = tx.execute("INSERT OR IGNORE INTO meeting_completion_notices(job_id,session_id) VALUES (?1,?2)",
        rusqlite::params![job.id, session]).map_err(|_| "無法儲存完成通知。")?;
    if inserted == 1 {
        let now = chrono::Utc::now().to_rfc3339();
        tx.execute("INSERT INTO session_messages(session_id,role,content,tokens,timestamp) VALUES (?1,'assistant',?2,0,?3)",
            rusqlite::params![session, text, now]).map_err(|_| "無法儲存完成通知。")?;
        // RM-173: a new message lifts an archive in the same transaction.
        tx.execute("UPDATE sessions SET last_active=?2, archived_at=NULL WHERE id=?1", rusqlite::params![session, now])
            .map_err(|_| "無法儲存完成通知。")?;
    }
    tx.commit().map_err(|_| "無法儲存完成通知。".into())
}

pub(crate) async fn deliver(home: &Path, job: &Job) -> Result<(), String> {
    let Some(reply) = job.reply_channel.as_deref() else { return Ok(()); };
    let channel = reply.split_once(':').map(|(channel, _)| channel).ok_or("無法辨識原對話。")?;
    if channel == "webchat" {
        return persist_webchat(home, job, &format_notice(&job.state, job.result.as_ref(), false)).await;
    }
    let descriptor = job.reply_target.as_ref().ok_or("此通道目前無法送回原對話，請至控制台查看逐字稿。")?;
    let target = crate::approval::TrustedReplyTarget::restore_notice(home, descriptor).await?;
    if target.context.channel != channel { return Err("通知通道與原對話不一致。".into()); }
    crate::product_policy::require_channel(home, channel)?;
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none()).build().map_err(|_| "無法建立通知連線。")?;
    let can_attach = crate::channel_capabilities::supports(channel, crate::channel_capabilities::Capability::FileUpload);
    let mut attached = false;
    if job.state == "done" && can_attach {
        if let Some(result) = &job.result {
            if let Some(path) = result["markdown_path"].as_str() {
                let agent_dir = crate::ephemeral::resolve_agent_dir(home, &job.agent)
                    .unwrap_or_else(|| home.join("agents").join(&job.agent));
                let bytes = duduclaw_inference::transcription::pipeline::read_attachment(&agent_dir, Path::new(path), 16 * 1024 * 1024);
                if let Ok(bytes) = bytes {
                    attached = target.send_notice_document(&http, &bytes, &output_name(result, "markdown_path")).await.is_ok();
                }
            }
        }
    }
    let text = format_notice(&job.state, job.result.as_ref(), attached);
    tokio::time::timeout(std::time::Duration::from_secs(30), target.send_watch_notice(&text)).await
        .map_err(|_| "傳送完成通知逾時，逐字稿仍可在控制台取得。")?
        .map_err(|_| "無法傳送完成通知，逐字稿仍可在控制台取得。".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn webchat_notice_is_durable_idempotent_and_bound_to_owner_and_employee() {
        let home = tempfile::tempdir().unwrap();
        let manager = crate::session::SessionManager::new(&home.path().join("sessions.db")).unwrap();
        let session = "webchat:notice-owner:conversation";
        manager.get_or_create_with_origin(session, "alice", &crate::session_origin::SessionOrigin::webchat_user("person")).await.unwrap();
        let mut job: Job = serde_json::from_value(serde_json::json!({
            "id":"notice-test-job", "agent":"alice", "source":"source", "source_bytes":1,
            "config":{}, "language":"zh-TW", "state":"failed", "chunk":0, "chunks":0,
            "created_at":0, "updated_at":0, "reply_channel":session, "reply_target":null,
            "requester":"person", "session":session, "result":null, "error":null,
            "cost_millicents":0,"cost_estimated":false,"reserved_bytes":0,"cleaned":true,"notified":false,
        })).unwrap();
        persist_webchat(home.path(), &job, "會議逐字稿未能完成。").await.unwrap();
        persist_webchat(home.path(), &job, "會議逐字稿未能完成。").await.unwrap();
        assert_eq!(manager.get_messages(session).await.unwrap().len(), 1);
        job.id = "other-job".into();
        job.agent = "bob".into();
        assert!(persist_webchat(home.path(), &job, "wrong employee").await.is_err());
        job.agent = "alice".into();
        job.requester = Some("someone-else".into());
        assert!(persist_webchat(home.path(), &job, "wrong owner").await.is_err());
        assert_eq!(manager.get_messages(session).await.unwrap().len(), 1);
    }

    #[test]
    fn notice_has_duration_speakers_filenames_and_honest_attachment_state() {
        let result = serde_json::json!({"duration_ms":7_205_000,"speaker_count":3,
            "markdown_path":"/private/secret/transcript.md","json_path":"/private/secret/transcript.json"});
        let text = format_notice("done", Some(&result), true);
        assert!(text.contains("120 分 5 秒"));
        assert!(text.contains("3 位"));
        assert!(text.contains("transcript.md、transcript.json"));
        assert!(!text.contains("/private/secret"));
        assert!(text.contains("已附上"));
        assert!(format_notice("done", Some(&result), false).contains("附件下載"));
        assert!(!format_notice("done", Some(&result), false).contains("已附上"));
    }

    #[test]
    fn failed_notice_never_echoes_provider_or_internal_error() {
        let result = serde_json::json!({"error":"HTTP 500 key=private-token"});
        let text = format_notice("failed", Some(&result), false);
        assert!(text.contains("未能完成"));
        assert!(!text.contains("500"));
        assert!(!text.contains("private-token"));
    }

    #[test]
    fn capture_refuses_unknown_and_other_employee_turns() {
        let _guard = crate::turn_context::register("notice-alice", "notice-turn", "telegram:42").unwrap();
        assert!(capture("notice-alice", Some("notice-turn")).reply_channel.is_none());
        let _requester = crate::knowledge::turn_requesters::register("notice-alice", "notice-turn",
            duduclaw_knowledge::requester::RequesterIdentity::DashboardUser { user_id:"person".into() }).unwrap();
        assert_eq!(capture("notice-alice", Some("notice-turn")).reply_channel.as_deref(), Some("telegram:42"));
        assert!(capture("notice-bob", Some("notice-turn")).reply_channel.is_none());
        assert!(capture("notice-alice", Some("unknown-turn")).reply_channel.is_none());
    }
}
