//! Durable per-file privacy, in addition to existing route/agent/task ACLs.
use super::audience::{dashboard_may_read, dashboard_may_read_task, task_audience};
use crate::workflow::WorkflowStore;
use std::path::Path;

pub async fn authorize_artifact_access(
    home: &Path,
    store: &WorkflowStore,
    ctx: &duduclaw_auth::UserContext,
    agent: Option<&str>,
    name: &str,
) -> Result<(), String> {
    if !crate::files_api::is_safe_filename(name) {
        return Err("permission denied".into());
    }
    let root = home.canonicalize().map_err(|_| "permission denied")?;
    let dir = crate::files_api::attachments_dir(home, agent).ok_or("permission denied")?;
    let expected = if let Some(a) = agent {
        root.join("agents").join(a).join("attachments")
    } else {
        root.join("attachments")
    };
    if dir.canonicalize().map_err(|_| "permission denied")? != expected {
        return Err("permission denied".into());
    }
    // V-M-1: the bindings below are keyed by name, so the name must be the
    // file itself. A symbolic link or a second hard link to a private archive
    // would carry its bytes under a name with no binding.
    match crate::files_api::resolve_attachment_file(&dir, name) {
        Ok(_) | Err(crate::files_api::ResolveError::NotFound) => {}
        Err(_) => return Err("permission denied".into()),
    }
    let index = crate::artifacts::HistoryIndex::load(home, agent);
    authorize_indexed(home, store, ctx, agent, name, index.provenance_checked(name)?, None).await
}

/// Projection uses one request-local durable index; the caller pins the attachment file.
pub(crate) async fn authorize_artifact_access_indexed(
    home: &Path, store: &WorkflowStore, ctx: &duduclaw_auth::UserContext,
    agent: Option<&str>, name: &str, index: &crate::artifacts::HistoryIndex,
) -> Result<(), String> {
    let provenance = index.provenance_checked(name)?;
    let content_bound = provenance.as_ref().is_some_and(|p| p.content_hash.is_some())
        || crate::workflow::artifact::has_committed_receipt(store, agent, name).await?;
    authorize_indexed(home, store, ctx, agent, name, provenance, Some(content_bound)).await
}

async fn authorize_indexed(
    home: &Path, store: &WorkflowStore, ctx: &duduclaw_auth::UserContext,
    agent: Option<&str>, name: &str, provenance: Option<crate::artifacts::FileProvenance>,
    content_verified: Option<bool>,
) -> Result<(), String> {
    if !crate::files_api::is_safe_filename(name) { return Err("permission denied".into()); }
    crate::workflow::artifact::verify_download_metadata(store, agent, name).await?;
    let agent_id = agent.unwrap_or("");
    let mut bindings = store
        .artifact_audiences(agent_id, name)
        .await
        .map_err(|_| "permission denied")?;
    let explicit_binding = !bindings.is_empty();
    // Legacy task attribution remains a restriction, but a basename-only
    // inferred row is never sufficient to grant a restricted reader bytes.
    if let Some(task)=provenance.as_ref().and_then(|p|p.task_id.as_ref()) {
        bindings.push((task.clone(),Vec::new()));
    }
    let fresh = super::audience::fresh_dashboard_context(home, ctx)?;
    if fresh.requires_password_change() {return Err("permission denied".into());}
    if let Some(agent)=agent {
        if crate::history_privacy::mode(home,&fresh,agent)?==crate::history_privacy::Mode::Filtered {
            // The same live authority decides both audience and content binding.
            if content_verified == Some(false) { return Err("permission denied".into()); }
            if !explicit_binding {
                let p=provenance.as_ref().ok_or("permission denied")?;
                let own=p.channel.as_deref()==Some("webchat") && p.owner_user_id.as_deref()==Some(fresh.user_id.as_str());
                if !own && !p.saved_task_archive {return Err("permission denied".into());}
            }
        }
    } else if !fresh.is_admin() {return Err("permission denied".into());}
    if bindings.is_empty() {return Ok(());}
    let ctx = &fresh;
    let task_store = crate::task_store::TaskStore::open(home).map_err(|_| "permission denied")?;
    for (task_id, audience) in bindings {
        let task = task_store
            .get_task(&task_id)
            .await
            .map_err(|_| "permission denied")?
            .ok_or("permission denied")?;
        let task_allowed = ctx.has_agent_access(
            &task.assigned_to,
            duduclaw_auth::models::AccessLevel::Viewer,
        );
        // Admins read through any AI-written audience; an unreadable packet
        // set denies everyone else.
        if !crate::task_store::TaskStore::a2a_principal_allows(home, &task_id, &format!("user:{}", ctx.user_id))
            || !task_allowed
            || !dashboard_may_read(ctx, &audience)
            || !dashboard_may_read_task(ctx, &task_audience(home, &task_id))
        {
            return Err("permission denied".into());
        }
    }
    Ok(())
}

/// The caller hashes a bounded snapshot read from the already-open FD and
/// serves those same bytes. Neither authorization nor receipts reopen a name.
pub(crate) async fn authorize_artifact_access_snapshot(
    home: &Path, store: &WorkflowStore, ctx: &duduclaw_auth::UserContext,
    agent: Option<&str>, name: &str, hash: &str, size: u64,
    index: &crate::artifacts::HistoryIndex,
) -> Result<(), String> {
    let provenance = index.provenance_checked(name)?;
    let content_verified = if let Some(expected) = provenance.as_ref().and_then(|p| p.content_hash.as_deref()) {
        if expected != hash || provenance.as_ref().is_none_or(|p| p.size != size) {
            return Err("permission denied".into());
        }
        true
    } else {
        // A workflow receipt supplies its own immutable content binding.
        crate::workflow::artifact::has_committed_receipt(store, agent, name).await?
    };
    crate::workflow::artifact::verify_download_hash(store, agent, name, hash, size).await?;
    authorize_indexed(home, store, ctx, agent, name, provenance, Some(content_verified)).await
}

/// Only used for private task archives. Privacy is durably committed first;
/// a failed copy leaves a restrictive tombstone, never a public orphan.
pub async fn save_private_artifact(
    home: &Path,
    task: &str,
    agent: &str,
    data: &[u8],
    display_name: &str,
    audience: &[String],
) -> Result<std::path::PathBuf, String> {
    use tokio::io::AsyncWriteExt;
    if data.len() as u64 > crate::media::MAX_FILE_SIZE { return Err("archive size limit exceeded".into()); }
    if audience.is_empty() {
        return Err("private artifact audience required".into());
    }
    let dir =
        crate::files_api::attachments_dir(home, Some(agent)).ok_or("invalid archive owner")?;
    let directory = crate::fs_safe::SafeDir::open_nofollow(&dir, true)
        .map_err(|_| "archive directory unavailable")?;
    let store = WorkflowStore::open(home)?;
    store.initialize_evidence().await?;
    let safe: String = display_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = duduclaw_core::truncate_bytes(&safe, 180);
    for _ in 0..8 {
        let name = format!("{}_{}", uuid::Uuid::new_v4(), safe);
        store
            .bind_artifact_audience(
                &uuid::Uuid::new_v4().to_string(),
                task,
                agent,
                &name,
                audience,
            )
            .await?;
        let path = dir.join(&name);
        match directory.open_create_new(&name) {
            Ok(file) => {
                let mut file = tokio::fs::File::from_std(file);
                let written = async {
                    file.write_all(data).await?;
                    file.sync_all().await
                }.await;
                if written.is_err() {
                    drop(file);
                    let _ = directory.remove_file(&name);
                    return Err("private archive write failed".into());
                }
                let recorded = crate::artifacts::record_saved_bytes(
                    &home.join("agents").join(agent), &path, display_name, data,
                    &crate::artifacts::SaveContext {
                        origin: crate::artifacts::ArtifactOrigin::Declared,
                        task_id: Some(task), round: None, channel: None, source_path: None,
                    },
                );
                if let Err(error) = recorded {
                    let _ = directory.remove_file(&name);
                    return Err(error);
                }
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("private archive create failed".into()),
        }
    }
    Err("private archive name collisions".into())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn private_archive_keeps_durable_audience_without_provenance_log() {
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir_all(home.path().join("agents/sales")).unwrap();
        let tasks = crate::task_store::TaskStore::open(home.path()).unwrap();
        let task = crate::task_store::TaskRow::new(
            "task".into(),
            "Report".into(),
            "Review staging".into(),
            "normal".into(),
            "sales".into(),
            "operator".into(),
        );
        tasks.insert_task(&task).await.unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let alice_user = db
            .create_user(
                "alice@test.invalid",
                "Alice",
                "isolated-test-password",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap();
        // F3 follow-up: an Admin reads through any audience, so the
        // outsider is a Manager bound to the employee (was a second Admin).
        let bob_user = db
            .create_user(
                "bob@test.invalid",
                "Bob",
                "isolated-test-password",
                duduclaw_auth::UserRole::Manager,
            )
            .unwrap();
        db.bind_agent(&bob_user.id, "sales", duduclaw_auth::AccessLevel::Viewer)
            .unwrap();
        let carol_user = db
            .create_user(
                "carol@test.invalid",
                "Carol",
                "isolated-test-password",
                duduclaw_auth::UserRole::Admin,
            )
            .unwrap();
        let saved = save_private_artifact(
            home.path(),
            "task",
            "sales",
            b"private report",
            "report.md",
            &[format!("user:{}", alice_user.id)],
        )
        .await
        .unwrap();
        let name = saved.file_name().unwrap().to_str().unwrap();
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        assert_eq!(
            store.artifact_audiences("sales", name).await.unwrap().len(),
            1
        );
        let mut alice = duduclaw_auth::UserContext::admin_fallback();
        alice.user_id = alice_user.id;
        let mut bob = alice.clone();
        bob.user_id = bob_user.id;
        bob.role = duduclaw_auth::UserRole::Manager;
        let mut carol = alice.clone();
        carol.user_id = carol_user.id;
        assert!(
            authorize_artifact_access(home.path(), &store, &carol, Some("sales"), name)
                .await
                .is_ok(),
            "an admin outside the audience still reads"
        );
        assert!(
            authorize_artifact_access(home.path(), &store, &alice, Some("sales"), name)
                .await
                .is_ok()
        );
        assert_eq!(
            authorize_artifact_access(home.path(), &store, &bob, Some("sales"), name)
                .await
                .unwrap_err(),
            "permission denied"
        );
        assert!(
            authorize_artifact_access(home.path(), &store, &alice, Some("sales"), "../report.md")
                .await
                .is_err()
        );
        drop(store);
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        assert!(
            authorize_artifact_access(home.path(), &store, &bob, Some("sales"), name)
                .await
                .is_err()
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn private_archive_refuses_every_symlinked_parent_without_outside_write() {
        for part in ["agents", "agents/sales", "agents/sales/attachments"] {
            let root = crate::fs_safe::private_test_home().unwrap();
            let home = root.path().canonicalize().unwrap();
            let outside = crate::fs_safe::private_test_home().unwrap();
            let planted = home.join(part);
            std::fs::create_dir_all(planted.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(outside.path(), planted).unwrap();
            assert!(save_private_artifact(&home, "task", "sales", b"private", "report.txt", &["user:alice".into()]).await.is_err());
            assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        }
    }

    /// V-M-1: a second name for a private archive (symbolic or hard link in
    /// the same attachments directory) is refused, not treated as unbound.
    #[cfg(unix)]
    #[tokio::test]
    async fn link_to_private_archive_under_new_name_is_refused() {
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir_all(home.path().join("agents/sales")).unwrap();
        let saved = save_private_artifact(
            home.path(),
            "task",
            "sales",
            b"private report",
            "report.md",
            &["user:alice".into()],
        )
        .await
        .unwrap();
        let dir = saved.parent().unwrap().to_path_buf();
        std::os::unix::fs::symlink(&saved, dir.join("public.md")).unwrap();
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        let viewer = duduclaw_auth::UserContext::admin_fallback();
        assert!(store.artifact_audiences("sales", "public.md").await.unwrap().is_empty());
        assert_eq!(
            authorize_artifact_access(home.path(), &store, &viewer, Some("sales"), "public.md")
                .await
                .unwrap_err(),
            "permission denied"
        );
        std::fs::hard_link(&saved, dir.join("copy.md")).unwrap();
        assert!(
            authorize_artifact_access(home.path(), &store, &viewer, Some("sales"), "copy.md")
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attachment_symlink_to_another_employee_is_refused() {
        let home = crate::fs_safe::private_test_home().unwrap();
        std::fs::create_dir_all(home.path().join("agents/sales")).unwrap();
        std::fs::create_dir_all(home.path().join("agents/other/attachments")).unwrap();
        std::os::unix::fs::symlink(
            home.path().join("agents/other/attachments"),
            home.path().join("agents/sales/attachments"),
        )
        .unwrap();
        assert!(
            save_private_artifact(
                home.path(),
                "task",
                "sales",
                b"private",
                "report.md",
                &["user:alice".into()]
            )
            .await
            .is_err()
        );
        assert!(
            std::fs::read_dir(home.path().join("agents/other/attachments"))
                .unwrap()
                .next()
                .is_none()
        );
    }
}

#[cfg(all(test, unix))]
mod bound_history_tests {
    use super::*;
    #[tokio::test]
    async fn conversation_archives_require_trusted_own_stamp_but_keep_owner_full() {
        use duduclaw_auth::{UserDb,UserRole,AccessLevel};
        let home=crate::fs_safe::private_test_home().unwrap();
        let db=UserDb::new(&home.path().join("users.db")).unwrap();
        let user=db.create_user("bound-files@test.invalid","viewer","isolated-password",UserRole::Employee).unwrap();
        db.bind_agent(&user.id,"sales",AccessLevel::Viewer).unwrap();
        let ctx=db.live_user_context(&user.id).unwrap().unwrap();
        let store=WorkflowStore::open(home.path()).unwrap();store.initialize_evidence().await.unwrap();
        let base=home.path().join("agents/sales");std::fs::create_dir_all(base.join("attachments")).unwrap();
        for name in ["own.txt","other.txt","external.txt","legacy.txt"] {std::fs::write(base.join("attachments").join(name),b"synthetic").unwrap();}
        for (name,principal,channel) in [("own.txt",user.id.as_str(),"webchat"),("other.txt","someone-else","webchat"),("external.txt",user.id.as_str(),"line")] {
            crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(principal.into(),async {
                crate::artifacts::record_saved(&base,&base.join("attachments").join(name),name,9,&crate::artifacts::SaveContext::uploaded(Some(channel)));
            }).await;
        }
        assert!(authorize_artifact_access(home.path(),&store,&ctx,Some("sales"),"own.txt").await.is_ok());
        for name in ["other.txt","external.txt","legacy.txt"] {assert!(authorize_artifact_access(home.path(),&store,&ctx,Some("sales"),name).await.is_err(),"{name}");}
        let tasks=crate::task_store::TaskStore::open(home.path()).unwrap();
        tasks.insert_task(&crate::task_store::TaskRow::new("bound-task".into(),"Synthetic".into(),"No private content".into(),"normal".into(),"sales".into(),"operator".into())).await.unwrap();
        for (name,source) in [("task-copy.txt",Some(std::path::Path::new("/synthetic/real-copy.txt"))),("inferred-name.txt",None)] {
            let path=base.join("attachments").join(name);std::fs::write(&path,b"synthetic").unwrap();
            crate::artifacts::record_saved(&base,&path,name,9,&crate::artifacts::SaveContext{origin:crate::artifacts::ArtifactOrigin::Swept,task_id:Some("bound-task"),round:None,channel:None,source_path:source});
        }
        assert!(authorize_artifact_access(home.path(),&store,&ctx,Some("sales"),"task-copy.txt").await.is_ok());
        assert!(authorize_artifact_access(home.path(),&store,&ctx,Some("sales"),"inferred-name.txt").await.is_err());
        db.bind_agent(&user.id,"sales",AccessLevel::Owner).unwrap();
        assert!(authorize_artifact_access(home.path(),&store,&ctx,Some("sales"),"other.txt").await.is_ok());
        db.bind_agent(&user.id,"sales",AccessLevel::Viewer).unwrap();
        std::fs::write(home.path().join("config.toml"),"[people]\nbound_history_privacy=false").unwrap();
        assert!(authorize_artifact_access(home.path(),&store,&ctx,Some("sales"),"legacy.txt").await.is_ok());
    }
}

#[cfg(all(test, unix))]
mod snapshot_tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::io::Read;

    #[tokio::test]
    async fn content_check_and_owner_check_use_the_same_provenance_row() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let alice = db.create_user("row-alice@test.invalid", "Alice", "isolated-password", duduclaw_auth::UserRole::Employee).unwrap();
        db.bind_agent(&alice.id, "sales", duduclaw_auth::AccessLevel::Viewer).unwrap();
        let ctx = db.live_user_context(&alice.id).unwrap().unwrap();
        let base = home.path().join("agents/sales");
        let path = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope("bob".into(), async {
            crate::media::save_webchat_attachment_in_base(&base, b"bob bytes", "report.txt").await.unwrap()
        }).await;
        let name = path.file_name().unwrap().to_str().unwrap();
        let index = crate::artifacts::HistoryIndex::load(home.path(), Some("sales"));
        let checked = index.provenance_checked(name).unwrap();
        assert_eq!(checked.as_ref().unwrap().content_hash.as_deref(), Some(hex::encode(Sha256::digest(b"bob bytes")).as_str()));
        // A replacement after the content check must not supply a different owner.
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(alice.id.clone(), async {
            crate::artifacts::record_saved_bytes(&base, &path, "report.txt", b"alice bytes", &crate::artifacts::SaveContext::uploaded(Some("webchat"))).unwrap();
        }).await;
        assert_eq!(index.provenance_checked(name).unwrap().unwrap().owner_user_id.as_deref(), Some(alice.id.as_str()));
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        assert!(authorize_indexed(home.path(), &store, &ctx, Some("sales"), name, checked, Some(true)).await.is_err());
    }

    #[tokio::test]
    async fn snapshot_authorization_binds_opened_bytes_after_path_replacement() {
        let home = crate::fs_safe::private_test_home().unwrap();
        let db = duduclaw_auth::UserDb::new(&home.path().join("users.db")).unwrap();
        let alice = db.create_user("snapshot-alice@test.invalid", "Alice", "isolated-password", duduclaw_auth::UserRole::Employee).unwrap();
        db.bind_agent(&alice.id, "sales", duduclaw_auth::AccessLevel::Viewer).unwrap();
        let ctx = db.live_user_context(&alice.id).unwrap().unwrap();
        let base = home.path().join("agents/sales");
        let path = crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope(alice.id.clone(), async {
            crate::media::save_webchat_attachment_in_base(&base, b"alice bytes", "report.txt").await.unwrap()
        }).await;
        let name = path.file_name().unwrap().to_str().unwrap();
        let store = WorkflowStore::open(home.path()).unwrap();
        store.initialize_evidence().await.unwrap();
        let (mut file, _) = crate::chat_attachments::open(home.path(), "sales", path.to_str().unwrap()).unwrap();
        let replacement = base.join("replacement.txt");
        std::fs::write(&replacement, b"bob bytes").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        let mut snapshot = Vec::new();
        file.read_to_end(&mut snapshot).unwrap();
        let index = crate::artifacts::HistoryIndex::load(home.path(), Some("sales"));
        let hash = hex::encode(Sha256::digest(&snapshot));
        assert!(authorize_artifact_access_snapshot(home.path(), &store, &ctx, Some("sales"), name, &hash, snapshot.len() as u64, &index).await.is_ok());
        assert!(authorize_artifact_access_indexed(home.path(), &store, &ctx, Some("sales"), name, &index).await.is_ok());
        let authority = rusqlite::Connection::open(home.path().join("artifact_provenance.db")).unwrap();
        let record: String = authority.query_row("SELECT record FROM artifact_provenance WHERE agent='sales' AND name=?1", [name], |r| r.get(0)).unwrap();
        let mut legacy_record: serde_json::Value = serde_json::from_str(&record).unwrap();
        legacy_record.as_object_mut().unwrap().remove("content_hash");
        authority.execute("UPDATE artifact_provenance SET record=?1 WHERE agent='sales' AND name=?2", rusqlite::params![legacy_record.to_string(), name]).unwrap();
        let legacy = crate::artifacts::HistoryIndex::load(home.path(), Some("sales"));
        assert!(authorize_artifact_access_indexed(home.path(), &store, &ctx, Some("sales"), name, &legacy).await.is_err());
        assert!(authorize_artifact_access_snapshot(home.path(), &store, &ctx, Some("sales"), name, &hash, snapshot.len() as u64, &legacy).await.is_err());
        db.bind_agent(&alice.id, "sales", duduclaw_auth::AccessLevel::Owner).unwrap();
        assert!(authorize_artifact_access_snapshot(home.path(), &store, &ctx, Some("sales"), name, &hash, snapshot.len() as u64, &legacy).await.is_ok());
        let stale_owner = db.live_user_context(&alice.id).unwrap().unwrap();
        db.bind_agent(&alice.id, "sales", duduclaw_auth::AccessLevel::Viewer).unwrap();
        assert!(authorize_artifact_access_snapshot(home.path(), &store, &stale_owner, Some("sales"), name, &hash, snapshot.len() as u64, &legacy).await.is_err());
        authority.execute("UPDATE artifact_provenance SET record=?1 WHERE agent='sales' AND name=?2", rusqlite::params![record, name]).unwrap();
        let replaced = std::fs::read(&path).unwrap();
        assert!(authorize_artifact_access_snapshot(home.path(), &store, &ctx, Some("sales"), name,
            &hex::encode(Sha256::digest(&replaced)), replaced.len() as u64, &index).await.is_err());
        // A new provenance row cannot authorize the FD opened under the old row.
        crate::ccr_runtime::AUTHENTICATED_WEBCHAT_PRINCIPAL.scope("bob".into(), async {
            crate::artifacts::record_saved_bytes(&base, &path, "report.txt", &replaced, &crate::artifacts::SaveContext::uploaded(Some("webchat"))).unwrap();
        }).await;
        let index = crate::artifacts::HistoryIndex::load(home.path(), Some("sales"));
        assert!(authorize_artifact_access_snapshot(home.path(), &store, &ctx, Some("sales"), name, &hash, snapshot.len() as u64, &index).await.is_err());
    }
}
