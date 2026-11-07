//! Stable, private report publication. File publication and SQL are a recoverable saga.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{io::Write, path::Path};

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
    let agent_id = agent.unwrap_or("");
    let records = download_receipts(store, agent, name).await?;
    if records.is_empty() { return check_pending_download(store, agent, name).await; }
    let root = crate::files_api::attachments_dir(home, Some(agent_id))
        .ok_or("workflow artifact owner refused")?;
    let path = crate::files_api::resolve_attachment_file(&root, name)
        .map_err(|_| "workflow artifact destination refused")?;
    let (hash, size) = file_hash(&path)?;
    verify_receipts(&records, &hash, size)
}

async fn download_receipts(store: &WorkflowStore, agent: Option<&str>, name: &str) -> Result<Vec<String>, String> {
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
    Ok(records)
}

async fn check_pending_download(store: &WorkflowStore, agent: Option<&str>, name: &str) -> Result<(), String> {
    let agent = agent.unwrap_or("");
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
    if pending {
        Err("workflow artifact commit pending".into())
    } else {
        Ok(())
    }
}

/// History metadata never exposes a pending or ambiguous workflow artifact.
pub(crate) async fn verify_download_metadata(store: &WorkflowStore, agent: Option<&str>, name: &str) -> Result<(), String> {
    let records = download_receipts(store, agent, name).await?;
    if records.is_empty() { return check_pending_download(store, agent, name).await; }
    if records.len() != 1 { return Err("workflow artifact receipt ambiguous".into()); }
    let receipt: Value = serde_json::from_str(&records[0]).map_err(|_| "workflow artifact receipt corrupt")?;
    if receipt.get("file_hash").and_then(Value::as_str).is_none()
        || receipt.get("bytes").and_then(Value::as_u64).is_none() {
        return Err("workflow artifact receipt corrupt".into());
    }
    Ok(())
}

pub(crate) async fn has_committed_receipt(store: &WorkflowStore, agent: Option<&str>, name: &str) -> Result<bool, String> {
    Ok(download_receipts(store, agent, name).await?.len() == 1)
}

pub(crate) async fn verify_download_hash(store: &WorkflowStore, agent: Option<&str>, name: &str, hash: &str, size: u64) -> Result<(), String> {
    let records = download_receipts(store, agent, name).await?;
    if records.is_empty() { return check_pending_download(store, agent, name).await; }
    verify_receipts(&records, hash, size)
}

fn verify_receipts(records: &[String], hash: &str, size: u64) -> Result<(), String> {
    if records.len() != 1 {
        return Err("workflow artifact receipt ambiguous".into());
    }
    let receipt: Value =
        serde_json::from_str(&records[0]).map_err(|_| "workflow artifact receipt corrupt")?;
    if receipt.get("file_hash").and_then(Value::as_str) != Some(hash)
        || receipt.get("bytes").and_then(Value::as_u64) != Some(size)
    {
        return Err("workflow artifact receipt no longer matches file".into());
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod download_snapshot_tests {
    use super::*;

    #[test]
    fn receipt_verifies_opened_snapshot_after_path_replacement() {
        use std::io::Read;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("report.json");
        std::fs::write(&path, b"original").unwrap();
        let mut opened = std::fs::File::open(&path).unwrap();
        let replacement = home.path().join("replacement.json");
        std::fs::write(&replacement, b"private replacement").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let mut snapshot = Vec::new();
        opened.read_to_end(&mut snapshot).unwrap();
        let hash = hex::encode(Sha256::digest(&snapshot));
        let receipts = vec![json!({"file_hash":hash,"bytes":snapshot.len()}).to_string()];
        assert!(verify_receipts(&receipts, &hash, snapshot.len() as u64).is_ok());
        let changed = std::fs::read(&path).unwrap();
        assert!(verify_receipts(&receipts, &hex::encode(Sha256::digest(&changed)), changed.len() as u64).is_err());
        assert!(verify_receipts(&receipts, &hash, 1).is_err());
        assert!(verify_receipts(&[receipts[0].clone(), receipts[0].clone()], &hash, snapshot.len() as u64).is_err());
    }

    #[tokio::test]
    async fn history_metadata_refuses_pending_and_ambiguous_workflow_receipts() {
        let home = tempfile::tempdir().unwrap();
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        store.bind_artifact_audience("workflow-artifact:run:step", "task", "sales", "report.json", &[]).await.unwrap();
        assert!(verify_download_metadata(&store, Some("sales"), "report.json").await.is_err());
        assert!(verify_download_hash(&store, Some("sales"), "report.json", "hash", 4).await.is_err());
        for id in ["first", "second"] {
            store.with_connection(|c| {
                c.execute("INSERT INTO workflow_artifact_commits VALUES (?1,'run','step','content','{}','[]',?2)",
                    rusqlite::params![id, json!({"agent_id":"sales","archived_name":"report.json","file_hash":"hash","bytes":4}).to_string()])
                    .map_err(|e| e.to_string())?;
                Ok(())
            }).await.unwrap();
            if id == "first" {
                assert!(verify_download_metadata(&store, Some("sales"), "report.json").await.is_ok());
                assert!(verify_download_hash(&store, Some("sales"), "report.json", "hash", 4).await.is_ok());
                assert!(verify_download_hash(&store, Some("sales"), "report.json", "changed", 4).await.is_err());
            }
        }
        assert!(verify_download_metadata(&store, Some("sales"), "report.json").await.is_err());
        assert!(verify_download_hash(&store, Some("sales"), "report.json", "hash", 4).await.is_err());
    }

}
