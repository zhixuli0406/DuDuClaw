//! Files a person hands over together with a task (`tasks.goal_create` and
//! `tasks.skill_assign`, the dashboard's 交辦任務 page).
//!
//! The request carries `attachments: [{name, mime, data_base64}]`. Every file
//! is decoded and size-checked **before anything is written or created**: one
//! bad entry (not base64, over the per-file cap, too many files, over the total
//! cap) refuses the whole request. Only then are the files saved into the
//! executing employee's own attachment area (`<home>/agents/<id>/attachments`,
//! the same place channel uploads land), and the saved absolute paths are
//! appended to the task description under a 「附件：」 line so the employee can
//! `Read` them.
//!
//! The dashboard RPC socket refuses frames over 1 MiB, so a request that
//! carries files reaches these handlers through `POST /api/tasks/rpc`
//! (`server.rs`), which runs the very same `MethodHandler::handle` dispatch —
//! authorisation is never decided twice.

use base64::Engine;
use serde_json::Value;

/// Most files one task may carry.
pub(crate) const MAX_TASK_ATTACHMENTS: usize = 10;

/// Per-file cap: the same 20 MiB every channel upload obeys.
pub(crate) const MAX_TASK_ATTACHMENT_BYTES: u64 = crate::media::MAX_FILE_SIZE;

/// All files of one task together.
pub(crate) const MAX_TASK_ATTACHMENTS_TOTAL_BYTES: u64 = 50 * 1024 * 1024;

/// Request body cap for `POST /api/tasks/rpc`: the total cap in base64
/// (4/3) plus room for the JSON envelope and the text fields.
pub(crate) const MAX_TASK_RPC_BODY_BYTES: usize =
    (MAX_TASK_ATTACHMENTS_TOTAL_BYTES as usize / 3 + 1) * 4 + 1024 * 1024;

/// Longest file name kept from the request (characters).
const MAX_NAME_CHARS: usize = 60;

/// The task description cap `goal_create_core` applies (characters).
pub(crate) const DESCRIPTION_MAX_CHARS: usize = 4000;

/// One decoded, size-checked file, not yet on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TaskAttachment {
    pub name: String,
    pub data: Vec<u8>,
}

/// Decode `params.attachments`. Absent or `null` ⇒ no files. Any malformed
/// entry refuses the whole list; the error is a caller-facing message.
pub(crate) fn decode_task_attachments(params: &Value) -> Result<Vec<TaskAttachment>, String> {
    let list = match params.get("attachments") {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(list)) => list,
        Some(_) => return Err("attachments must be a list".to_string()),
    };
    if list.len() > MAX_TASK_ATTACHMENTS {
        return Err(format!(
            "attachments_too_many: at most {MAX_TASK_ATTACHMENTS} files per task"
        ));
    }
    // Base64 length bound checked before decoding, so an oversized string is
    // refused without allocating its decoded copy.
    let max_b64_len = (MAX_TASK_ATTACHMENT_BYTES as usize / 3 + 1) * 4;
    // The same bound over all files together, before any of them is decoded:
    // otherwise the file that crosses the total cap is decoded first.
    let max_total_b64_len = (MAX_TASK_ATTACHMENTS_TOTAL_BYTES as usize / 3 + 1) * 4;
    let total_b64_len: usize = list
        .iter()
        .filter_map(|e| e.get("data_base64").and_then(Value::as_str))
        .map(str::len)
        .sum();
    if total_b64_len > max_total_b64_len {
        return Err(format!(
            "attachments_too_large: all files together are over {} MB",
            MAX_TASK_ATTACHMENTS_TOTAL_BYTES / (1024 * 1024)
        ));
    }
    let mut total: u64 = 0;
    let mut out = Vec::with_capacity(list.len());
    for (i, entry) in list.iter().enumerate() {
        let n = i + 1;
        let b64 = entry
            .get("data_base64")
            .and_then(Value::as_str)
            .ok_or_else(|| format!("attachments_invalid: file {n} has no data_base64"))?;
        if b64.len() > max_b64_len {
            return Err(format!(
                "attachments_too_large: file {n} is over {} MB",
                MAX_TASK_ATTACHMENT_BYTES / (1024 * 1024)
            ));
        }
        let data = base64::engine::general_purpose::STANDARD
            .decode(b64.as_bytes())
            .map_err(|_| format!("attachments_invalid: file {n} is not valid base64"))?;
        if data.len() as u64 > MAX_TASK_ATTACHMENT_BYTES {
            return Err(format!(
                "attachments_too_large: file {n} is over {} MB",
                MAX_TASK_ATTACHMENT_BYTES / (1024 * 1024)
            ));
        }
        total += data.len() as u64;
        if total > MAX_TASK_ATTACHMENTS_TOTAL_BYTES {
            return Err(format!(
                "attachments_too_large: all files together are over {} MB",
                MAX_TASK_ATTACHMENTS_TOTAL_BYTES / (1024 * 1024)
            ));
        }
        out.push(TaskAttachment {
            name: clean_name(entry.get("name").and_then(Value::as_str)),
            data,
        });
    }
    Ok(out)
}

/// A display-safe base name: no directory part, no control characters, capped.
/// The on-disk name is sanitised again by `media::save_attachment_in_base`.
fn clean_name(raw: Option<&str>) -> String {
    let base = raw
        .unwrap_or("")
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    let base = base.trim().trim_start_matches('.');
    if base.is_empty() {
        return "upload.bin".to_string();
    }
    duduclaw_core::truncate_chars(base, MAX_NAME_CHARS)
}

/// Save every file into `agent_id`'s attachment area. On any failure the files
/// already written are removed again and the error is returned.
pub(crate) async fn save_task_attachments(
    home_dir: &std::path::Path,
    agent_id: &str,
    files: &[TaskAttachment],
) -> Result<Vec<std::path::PathBuf>, String> {
    let base = crate::media::agent_attachment_base(home_dir, agent_id);
    let mut saved = Vec::with_capacity(files.len());
    for (i, f) in files.iter().enumerate() {
        // The index keeps two same-named files saved in the same millisecond
        // apart (the store names files `<ms>_<name>`).
        let name = format!("{}_{}", i + 1, f.name);
        // Untracked save plus an explicit `uploaded` row: the tracked
        // variant also registers the file for the chat 「加入知識庫」 claim,
        // which is for files a chat message carried, not dashboard tasks.
        match crate::media::save_attachment_in_base_untracked(&base, &f.data, &name).await {
            Ok(path) => {
                crate::artifacts::record_saved(
                    &base,
                    &path,
                    &name,
                    f.data.len() as u64,
                    &crate::artifacts::SaveContext::uploaded(Some("dashboard")),
                );
                saved.push(path);
            }
            Err(e) => {
                remove_saved(&saved).await;
                // The OS error names host paths; keep it in the log only.
                tracing::warn!(agent = %agent_id, "task attachment save failed: {e}");
                return Err("attachments_save_failed: the file could not be saved".to_string());
            }
        }
    }
    Ok(saved)
}

/// Best-effort removal of files saved for a request that then failed.
pub(crate) async fn remove_saved(paths: &[std::path::PathBuf]) {
    for p in paths {
        if let Err(e) = tokio::fs::remove_file(p).await {
            tracing::warn!(path = %p.display(), error = %e, "could not remove an attachment of a refused task");
        }
    }
}

/// A saved file: the name the person gave it and where it now is.
pub(crate) type SavedAttachment = (String, std::path::PathBuf);

/// Pair each file's original name with its saved path (the on-disk name is
/// ASCII-sanitised, so a 「名單.csv」 would otherwise reach the employee as
/// `__.csv`).
pub(crate) fn named(
    files: &[TaskAttachment],
    paths: &[std::path::PathBuf],
) -> Vec<SavedAttachment> {
    files
        .iter()
        .zip(paths)
        .map(|(f, p)| (f.name.clone(), p.clone()))
        .collect()
}

/// The 「附件：」 block appended to a task description.
pub(crate) fn attachments_block(entries: &[SavedAttachment]) -> String {
    let mut s = String::from("附件（已放進你的檔案區，請用絕對路徑讀取）：");
    for (name, path) in entries {
        s.push_str(&format!("\n- {name}：{}", path.display()));
    }
    s
}

/// The 附件 block may take at most half the description, so the goal text
/// (or the skill header) always keeps room. Only very long home paths can hit
/// this with ten files; the request is then refused instead of cut.
pub(crate) fn check_block_fits(entries: &[SavedAttachment]) -> Result<(), String> {
    if attachments_block(entries).chars().count() > DESCRIPTION_MAX_CHARS / 2 {
        return Err(
            "attachments_too_many: the file list does not fit the task description".to_string(),
        );
    }
    Ok(())
}

/// `text` followed by `block`, with `text` shortened so the whole result
/// stays inside the description cap and the block is never the part cut off.
pub(crate) fn append_block(text: &str, block: &str) -> String {
    if block.is_empty() {
        return text.to_string();
    }
    let room = DESCRIPTION_MAX_CHARS.saturating_sub(block.chars().count() + 2);
    format!(
        "{}\n\n{block}",
        duduclaw_core::truncate_chars(text.trim(), room)
    )
}

impl super::MethodHandler {
    /// `tasks.goal_create` params with any `attachments` saved into
    /// `agent_id`'s attachment area and listed at the end of `description`.
    /// Returns the rewritten params (without the file bodies) and the saved
    /// paths, so the caller can remove them if the goal is then refused.
    /// The caller has already authorised assigning to `agent_id`.
    pub(crate) async fn goal_params_with_attachments(
        &self,
        agent_id: &str,
        mut params: Value,
    ) -> Result<(Value, Vec<std::path::PathBuf>), String> {
        let files = decode_task_attachments(&params)?;
        if let Some(obj) = params.as_object_mut() {
            obj.remove("attachments");
        }
        if files.is_empty() {
            return Ok((params, Vec::new()));
        }
        let description = params
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        // Same refusal `goal_create_core` would give, but before any file is
        // written for a goal that cannot exist.
        if description.is_empty() {
            return Err("description is required".to_string());
        }
        // Never create an attachment folder for an employee that does not
        // exist (it would look like a half-made employee directory).
        if self.registry.read().await.get(agent_id).is_none() {
            return Err(format!("Agent not found: {agent_id}"));
        }
        let saved = save_task_attachments(&self.home_dir, agent_id, &files).await?;
        let entries = named(&files, &saved);
        if let Err(e) = check_block_fits(&entries) {
            remove_saved(&saved).await;
            return Err(e);
        }
        let block = attachments_block(&entries);
        // Without explicit criteria the goal text becomes the acceptance
        // baseline, and the ledger splits it per line: pin the criteria to the
        // text the person wrote so the file list never turns into criteria.
        let criteria_given = params
            .get("acceptance_criteria")
            .and_then(Value::as_str)
            .is_some_and(|c| !c.trim().is_empty());
        if !criteria_given {
            params["acceptance_criteria"] = Value::String(description.clone());
        }
        params["description"] = Value::String(append_block(&description, &block));
        Ok((params, saved))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn b64(bytes: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    #[test]
    fn no_attachments_field_means_no_files() {
        assert!(decode_task_attachments(&json!({})).unwrap().is_empty());
        assert!(decode_task_attachments(&json!({ "attachments": null }))
            .unwrap()
            .is_empty());
        assert!(decode_task_attachments(&json!({ "attachments": [] }))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn decodes_and_cleans_names() {
        let files = decode_task_attachments(&json!({ "attachments": [
            { "name": "../../etc/報價單.pdf", "mime": "application/pdf", "data_base64": b64(b"hi") },
            { "name": "", "data_base64": b64(b"x") },
            { "name": "..hidden", "data_base64": b64(b"y") },
        ]}))
        .unwrap();
        assert_eq!(files[0].name, "報價單.pdf");
        assert_eq!(files[0].data, b"hi");
        assert_eq!(files[1].name, "upload.bin");
        assert_eq!(files[2].name, "hidden");
    }

    #[test]
    fn refuses_bad_base64_too_many_and_wrong_shape() {
        let bad = decode_task_attachments(&json!({ "attachments": [
            { "name": "a", "data_base64": b64(b"ok") },
            { "name": "b", "data_base64": "%%%not base64%%%" },
        ]}));
        assert!(bad.unwrap_err().starts_with("attachments_invalid"));
        let missing = decode_task_attachments(&json!({ "attachments": [{ "name": "a" }] }));
        assert!(missing.unwrap_err().starts_with("attachments_invalid"));
        let many: Vec<Value> = (0..=MAX_TASK_ATTACHMENTS)
            .map(|i| json!({ "name": format!("{i}.txt"), "data_base64": b64(b"x") }))
            .collect();
        let too_many = decode_task_attachments(&json!({ "attachments": many }));
        assert!(too_many.unwrap_err().starts_with("attachments_too_many"));
        assert!(decode_task_attachments(&json!({ "attachments": "x" })).is_err());
    }

    #[test]
    fn refuses_a_file_over_the_per_file_cap() {
        let big = vec![0u8; MAX_TASK_ATTACHMENT_BYTES as usize + 1];
        let err = decode_task_attachments(&json!({ "attachments": [
            { "name": "big.bin", "data_base64": b64(&big) },
        ]}))
        .unwrap_err();
        assert!(err.starts_with("attachments_too_large"), "{err}");
    }

    #[test]
    fn refuses_files_over_the_total_cap() {
        let chunk = vec![0u8; MAX_TASK_ATTACHMENT_BYTES as usize];
        let entry = json!({ "name": "c.bin", "data_base64": b64(&chunk) });
        let err = decode_task_attachments(
            &json!({ "attachments": [entry.clone(), entry.clone(), entry] }),
        )
        .unwrap_err();
        assert!(err.contains("all files together"), "{err}");
    }

    #[test]
    fn the_total_cap_is_checked_before_anything_is_decoded() {
        // Each string is under the per-file bound but they are not base64:
        // decoding any of them would answer `attachments_invalid`.
        let junk = "%".repeat((MAX_TASK_ATTACHMENT_BYTES as usize / 3) * 4);
        let entry = json!({ "name": "j.bin", "data_base64": junk });
        let err = decode_task_attachments(
            &json!({ "attachments": [entry.clone(), entry.clone(), entry] }),
        )
        .unwrap_err();
        assert!(err.contains("all files together"), "{err}");
    }

    #[test]
    fn the_block_survives_a_long_description() {
        let paths = vec![(
            "報價.pdf".to_string(),
            std::path::PathBuf::from("/h/agents/a/attachments/1_x.pdf"),
        )];
        let block = attachments_block(&paths);
        assert!(block.contains("- 報價.pdf：/h/agents/a/attachments/1_x.pdf"));
        let long = "字".repeat(DESCRIPTION_MAX_CHARS);
        let out = append_block(&long, &block);
        assert!(out.chars().count() <= DESCRIPTION_MAX_CHARS);
        assert!(out.ends_with("/h/agents/a/attachments/1_x.pdf"));
        assert_eq!(append_block("做事", ""), "做事");
    }

    #[test]
    fn a_file_list_longer_than_half_the_description_is_refused() {
        let long = "/very/long/home".repeat(20);
        let entries: Vec<SavedAttachment> = (0..10)
            .map(|i| {
                (
                    format!("{i}.pdf"),
                    std::path::PathBuf::from(format!("{long}/{i}.pdf")),
                )
            })
            .collect();
        assert!(check_block_fits(&entries)
            .unwrap_err()
            .starts_with("attachments_too_many"));
        assert!(check_block_fits(&entries[..1]).is_ok());
    }

    #[tokio::test]
    async fn saves_into_the_agents_own_attachment_area() {
        let home = tempfile::tempdir().unwrap();
        let files = vec![
            TaskAttachment {
                name: "a.txt".into(),
                data: b"1".to_vec(),
            },
            TaskAttachment {
                name: "a.txt".into(),
                data: b"2".to_vec(),
            },
        ];
        let saved = save_task_attachments(home.path(), "sales", &files)
            .await
            .unwrap();
        assert_eq!(saved.len(), 2);
        assert_ne!(
            saved[0], saved[1],
            "same-named files never overwrite each other"
        );
        let dir = home.path().join("agents").join("sales").join("attachments");
        for p in &saved {
            assert!(p.starts_with(&dir), "{}", p.display());
        }
        assert_eq!(std::fs::read(&saved[1]).unwrap(), b"2");
        remove_saved(&saved).await;
        assert!(saved.iter().all(|p| !p.exists()));
    }
}
