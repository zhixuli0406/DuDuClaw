//! Stable, private report publication. File publication and SQL are a recoverable saga.
use super::*;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

fn file_hash(path: &Path) -> Result<(String, u64), String> {
    let m = std::fs::symlink_metadata(path).map_err(|_| "workflow artifact unavailable")?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > schema::MAX_JSON_BYTES as u64 {
        return Err("workflow artifact file refused".into());
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    options
        .open(path)
        .map_err(|_| "workflow artifact unreadable")?
        .take(schema::MAX_JSON_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "workflow artifact unreadable")?;
    if bytes.len() > schema::MAX_JSON_BYTES {
        return Err("workflow artifact bound exceeded".into());
    }
    Ok((hex::encode(Sha256::digest(&bytes)), bytes.len() as u64))
}
fn private_directory(path: &Path, expected: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(path).map_err(|_| "workflow artifact directory unavailable")?;
        }
        Ok(m) if m.is_dir() && !m.file_type().is_symlink() => (),
        _ => return Err("workflow artifact directory refused".into()),
    }
    if path
        .canonicalize()
        .map_err(|_| "workflow artifact directory unavailable")?
        != expected
    {
        return Err("workflow artifact directory outside owner".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| "workflow artifact directory not private")?;
    }
    Ok(())
}
pub async fn publish(
    home: &Path,
    store: &WorkflowStore,
    run: &WorkflowRun,
    step: &StepDefinition,
    input: &Value,
    label: &str,
) -> Result<Value, String> {
    if !duduclaw_core::is_valid_agent_id(&run.actor)
        || uuid::Uuid::parse_str(&run.run_id).is_err()
        || !duduclaw_core::is_valid_agent_id(&step.step_id)
    {
        return Err("workflow artifact identity refused".into());
    }
    let home = home
        .canonicalize()
        .map_err(|_| "workflow artifact home unavailable")?;
    let owner = home.join("agents").join(&run.actor);
    if owner
        .canonicalize()
        .map_err(|_| "workflow artifact owner unavailable")?
        != owner
    {
        return Err("workflow artifact owner refused".into());
    }
    let archive = crate::files_api::attachments_dir(&home, Some(&run.actor))
        .ok_or("workflow artifact owner refused")?;
    private_directory(&archive, &owner.join("attachments"))?;
    let temporary = owner.join("workflow-tmp");
    private_directory(&temporary, &temporary)?;
    let name = format!("wf-{}-{}.json", run.run_id, step.step_id);
    if !crate::files_api::is_safe_filename(&name) {
        return Err("workflow artifact filename refused".into());
    }
    let bytes = duduclaw_core::workflow_mcp::canonical_bytes(input)?;
    if bytes.len() > schema::MAX_JSON_BYTES {
        return Err("workflow artifact bound exceeded".into());
    }
    let hash = hex::encode(Sha256::digest(&bytes));
    let path = archive.join(&name);
    let draft = store
        .draft_for_workflow(&run.workflow_id, run.revision)
        .await?
        .ok_or("workflow artifact source draft missing")?;
    // Privacy commits before publication. A failed publish leaves a restrictive tombstone.
    store
        .bind_artifact_audience(
            &format!("workflow-artifact:{}:{}", run.run_id, step.step_id),
            &draft.source_task,
            &run.actor,
            &name,
            &run.audience,
        )
        .await?;
    match std::fs::symlink_metadata(&path) {
        Ok(_) => {
            if file_hash(&path) != (Ok((hash.clone(), bytes.len() as u64))) {
                return Err("workflow artifact existing bytes differ".into());
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let temp = temporary.join(format!("{}.tmp", uuid::Uuid::new_v4()));
            let outcome = (|| -> Result<(), String> {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
                }
                let mut file = options
                    .open(&temp)
                    .map_err(|_| "workflow artifact private write failed")?;
                file.write_all(&bytes)
                    .map_err(|_| "workflow artifact write failed")?;
                file.sync_all()
                    .map_err(|_| "workflow artifact sync failed")?;
                match std::fs::hard_link(&temp, &path) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(_) => return Err("workflow artifact atomic publish failed".into()),
                }
                if file_hash(&path) != (Ok((hash.clone(), bytes.len() as u64))) {
                    return Err("workflow artifact publish readback differs".into());
                }
                #[cfg(unix)]
                {
                    std::fs::File::open(&archive)
                        .and_then(|d| d.sync_all())
                        .map_err(|_| "workflow artifact directory sync failed")?;
                }
                Ok(())
            })();
            let _ = std::fs::remove_file(temp);
            outcome?;
        }
        Err(_) => return Err("workflow artifact destination unavailable".into()),
    }
    Ok(
        json!({
            "version": 1,
            "record_id": format!("{}:{}",run.run_id,step.step_id),
            "run_id": run.run_id,
            "step_id": step.step_id,
            "content_hash": crate::approval::payload_hash(input),
            "label": label,
            "audience": run.audience,
            "agent_id": run.actor,
            "archived_name": name,
            "path": format!("agents/{}/attachments/{}",run.actor,name),
            "file_hash": hash,
            "bytes": bytes.len()
        }),
    )
}
/// Pending artifacts are not served. Committed artifacts must still match their receipt.
pub async fn verify_download(
    home: &Path,
    store: &WorkflowStore,
    agent: Option<&str>,
    name: &str,
) -> Result<(), String> {
    let agent = agent.unwrap_or("");
    let records: Vec<String> = store
        .with_connection(|c| {
            let mut q = c
                .prepare("SELECT receipt_json FROM workflow_artifact_commits WHERE json_extract(receipt_json,
                    '$.agent_id')=?1 AND json_extract(receipt_json,'$.archived_name')=?2")
                .map_err(|e| e.to_string())?;
            q.query_map([agent, name], |r| r.get(0))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())
        })
        .await?;
    if records.is_empty() {
        let pending: bool = store
            .with_connection(|c|
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM review_artifact_audiences WHERE agent_id=?1 AND archived_name=?2
                        AND snapshot_id GLOB 'workflow-artifact:*')",
                    [agent, name],
                    |r| r.get(0)
                )
                .map_err(|e| e.to_string())
            )
            .await?;
        return if pending {
            Err("workflow artifact commit pending".into())
        } else {
            Ok(())
        };
    }
    if records.len() != 1 {
        return Err("workflow artifact receipt ambiguous".into());
    }
    let receipt: Value =
        serde_json::from_str(&records[0]).map_err(|_| "workflow artifact receipt corrupt")?;
    let root = crate::files_api::attachments_dir(home, Some(agent))
        .ok_or("workflow artifact owner refused")?;
    let path = crate::files_api::resolve_attachment_file(&root, name)
        .map_err(|_| "workflow artifact destination refused")?;
    let (hash, size) = file_hash(&path)?;
    if receipt.get("file_hash").and_then(Value::as_str) != Some(hash.as_str())
        || receipt.get("bytes").and_then(Value::as_u64) != Some(size)
    {
        return Err("workflow artifact receipt no longer matches file".into());
    }
    Ok(())
}
