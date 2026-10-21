//! `agent_file_change`: a non-admin's profile file change that the check
//! blocked (or that they chose not to confirm) waits for an admin.
//!
//! Dashboard-only (`approval_notify::is_dashboard_only_kind`): channels get a
//! plain notice without buttons and a channel decision is always refused.
//! The proposed text lives in a sidecar `<home>/agent_file_pending/<id>.txt`
//! (owner-only); the approval payload carries only hashes, who asked, why and
//! the check result. On approve the gateway applies first and records the
//! decision only after the write succeeded; a file that changed while the
//! request waited is refused (`base_hash`) and the request denied. Every
//! terminal state removes the sidecar (lazily for expiry, see [`sweep`]).

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::FileKey;
use super::verdict::{Analysis, Verdict};
use crate::approval::{ApprovalBroker, ApprovalId, ApprovalRecord, ApprovalStatus};

pub(crate) const ACTION_KIND: &str = "agent_file_change";
/// Three days; expiry counts as a denial.
pub(crate) const TTL_SECS: i64 = 3 * 24 * 3600;

/// The plain zh-TW name of a file (channel notices, summaries).
pub(crate) fn plain_name(key: FileKey) -> &'static str {
    match key {
        FileKey::Soul => "人格設定",
        FileKey::Identity => "自我介紹",
        FileKey::Instructions => "工作規則",
        FileKey::Contract => "行為界線",
        FileKey::AgentToml => "基本設定",
    }
}

fn pending_dir(home: &Path) -> PathBuf {
    home.join("agent_file_pending")
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn sidecar(home: &Path, id: &str) -> Result<PathBuf, String> {
    if !valid_id(id) {
        return Err("invalid approval id".into());
    }
    Ok(pending_dir(home).join(format!("{id}.txt")))
}

pub(crate) fn read_sidecar(home: &Path, id: &str) -> Result<String, String> {
    std::fs::read_to_string(sidecar(home, id)?).map_err(|_| "the proposed content is no longer stored".to_string())
}

pub(crate) fn remove_sidecar(home: &Path, id: &str) {
    if let Ok(p) = sidecar(home, id) {
        let _ = std::fs::remove_file(p);
    }
}

fn write_sidecar(home: &Path, id: &str, content: &str) -> Result<(), String> {
    let dir = pending_dir(home);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot store the proposed content: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    }
    crate::handlers::config_commit::write_owner_only(&sidecar(home, id)?, content)
        .map_err(|e| format!("cannot store the proposed content: {e}"))
}

/// The request's (agent, file) when `rec` is one of ours.
pub(crate) fn target(rec: &ApprovalRecord) -> Option<(String, FileKey)> {
    let agent = rec.payload.get("agent_id")?.as_str()?.to_string();
    let file = FileKey::parse(rec.payload.get("file")?.as_str()?).ok()?;
    Some((agent, file))
}

fn live_pending(rec: &ApprovalRecord) -> bool {
    rec.status == ApprovalStatus::Pending && !rec.is_stale(chrono::Utc::now())
}

/// File the request. A previous waiting request by the same person for the
/// same file is withdrawn first (the new one supersedes it).
pub(crate) async fn file_request(
    home: &Path,
    analysis: &Analysis,
    content: &str,
    requested_by_email: &str,
    reason: &str,
) -> Result<String, String> {
    let broker = ApprovalBroker::open(home)?;
    for rec in broker.list_by_kind(ACTION_KIND).await? {
        let mine = rec.payload.get("requested_by").and_then(Value::as_str) == Some(analysis.user_id.as_str());
        if live_pending(&rec) && mine && target(&rec) == Some((analysis.agent_id.clone(), analysis.file)) {
            broker.withdraw(&rec.id, "superseded").await?;
            remove_sidecar(home, rec.id.as_str());
        }
    }
    let categories = super::verdict::categories(&analysis.reasons);
    let payload = json!({
        "agent_id": analysis.agent_id,
        "file": analysis.file.as_str(),
        "base_hash": analysis.base_hash,
        "content_sha256": analysis.content_sha256,
        "requested_by": analysis.user_id,
        "requested_by_email": requested_by_email,
        "verdict": analysis.verdict.as_str(),
        "reason_categories": categories,
        "reason": duduclaw_core::truncate_chars(reason, 1000),
    });
    let summary = format!(
        "{who} 想修改 AI 員工「{agent}」的{file}；自動檢查結果：{result}。",
        who = duduclaw_core::truncate_chars(requested_by_email, 80),
        agent = analysis.agent_id,
        file = plain_name(analysis.file),
        result = match analysis.verdict {
            Verdict::Block => "高風險",
            Verdict::Warn => "有疑慮",
            Verdict::Pass => "沒有發現問題",
        },
    );
    let id = broker.request(&analysis.agent_id, ACTION_KIND, &summary, payload, TTL_SECS).await?;
    if let Err(e) = write_sidecar(home, id.as_str(), content) {
        let _ = broker.withdraw(&id, "sidecar_write_failed").await;
        return Err(e);
    }
    crate::security_autopilot::audit_and_emit(
        home,
        &duduclaw_security::audit::AuditEvent::new(
            "agent_file_change_requested",
            analysis.agent_id.as_str(),
            duduclaw_security::audit::Severity::Info,
            json!({
                "agent_id": analysis.agent_id,
                "file": analysis.file.as_str(),
                "user_id": analysis.user_id,
                "approval_id": id.as_str(),
                "verdict": analysis.verdict.as_str(),
                "reason_categories": categories,
                "base_hash": analysis.base_hash,
                "content_sha256": analysis.content_sha256,
            }),
        ),
    );
    Ok(id.as_str().to_string())
}

/// The newest live request for (agent, file), if any.
pub(crate) async fn pending_for(home: &Path, agent_id: &str, key: FileKey) -> Option<String> {
    let broker = ApprovalBroker::open(home).ok()?;
    broker
        .list_by_kind(ACTION_KIND)
        .await
        .ok()?
        .into_iter()
        .filter(|r| live_pending(r) && target(r) == Some((agent_id.to_string(), key)))
        .max_by(|a, b| a.created_at.cmp(&b.created_at))
        .map(|r| r.id.as_str().to_string())
}

/// Remove sidecars whose request is no longer waiting (expired, decided,
/// withdrawn or gone). Cheap; called from the agent_files RPCs.
pub(crate) async fn sweep(home: &Path) {
    let Ok(entries) = std::fs::read_dir(pending_dir(home)) else { return };
    let Ok(broker) = ApprovalBroker::open(home) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(id) = name.strip_suffix(".txt").filter(|i| valid_id(i)) else { continue };
        let keep = matches!(broker.get(&ApprovalId::from(id.to_string())).await, Ok(Some(r)) if live_pending(&r));
        if !keep {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// `agent_files.pending_get` body.
pub(crate) fn pending_json(home: &Path, rec: &ApprovalRecord) -> Result<Value, String> {
    let (agent_id, key) = target(rec).ok_or("not a profile file change request")?;
    let p = &rec.payload;
    let s = |k: &str| p.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    let content = read_sidecar(home, rec.id.as_str()).ok();
    let path = super::file_path(home, &agent_id, key)?;
    let current = super::read_or_empty(&path)?;
    let current_hash = super::hash_of(&current);
    let content_ok = content.as_deref().is_some_and(|c| super::hash_of(c) == s("content_sha256"));
    Ok(json!({
        "approval_id": rec.id.as_str(),
        "agent_id": agent_id,
        "file": key.as_str(),
        "status": rec.status.as_str(),
        "requested_by": if s("requested_by_email").is_empty() { s("requested_by") } else { s("requested_by_email") },
        "reason": s("reason"),
        "verdict": s("verdict"),
        "reason_categories": p.get("reason_categories").cloned().unwrap_or_else(|| json!([])),
        "stale": s("base_hash") != current_hash,
        "content_available": content_ok,
        "diff": content.filter(|_| content_ok).map(|c| super::diff::diff_json(&current, &c)),
    }))
}

/// The proposed content of an approved request, verified against the hash
/// in the payload.
pub(crate) fn approved_content(home: &Path, rec: &ApprovalRecord) -> Result<String, String> {
    let content = read_sidecar(home, rec.id.as_str())?;
    let expected = rec.payload.get("content_sha256").and_then(Value::as_str).unwrap_or("");
    if super::hash_of(&content) != expected {
        return Err("the stored proposed content does not match the request".into());
    }
    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidecar_ids_are_strict() {
        assert!(valid_id("0b5e7c1a-2f9d-4c3e-8a77-1234567890ab"));
        for bad in ["", "../x", "a/b", "a.txt", &"x".repeat(65)] {
            assert!(!valid_id(bad), "{bad}");
        }
    }
}
