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
    let agent_id = agent.unwrap_or("");
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
    let mut bindings = store
        .artifact_audiences(agent_id, name)
        .await
        .map_err(|_| "permission denied")?;
    let explicit_binding = !bindings.is_empty();
    let index=crate::artifacts::provenance_index(home,agent);
    let provenance=index.get(name);
    // Legacy task attribution remains a restriction, but a basename-only
    // inferred row is never sufficient to grant a restricted reader bytes.
    if let Some(task)=provenance.and_then(|p|p.task_id.as_ref()) {
        bindings.push((task.clone(),Vec::new()));
    }
    let fresh = super::audience::fresh_dashboard_context(home, ctx)?;
    if fresh.requires_password_change() {return Err("permission denied".into());}
    if let Some(agent)=agent {
        if crate::history_privacy::mode(home,&fresh,agent)?==crate::history_privacy::Mode::Filtered && !explicit_binding {
            let p=provenance.ok_or("permission denied")?;
            let own=p.channel.as_deref()==Some("webchat") && p.owner_user_id.as_deref()==Some(fresh.user_id.as_str());
            if !own && !p.saved_task_archive {return Err("permission denied".into());}
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
        if !task_allowed
            || !dashboard_may_read(ctx, &audience)
            || !dashboard_may_read_task(ctx, &task_audience(home, &task_id))
        {
            return Err("permission denied".into());
        }
    }
    Ok(())
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
    if audience.is_empty() {
        return Err("private artifact audience required".into());
    }
    let dir =
        crate::files_api::attachments_dir(home, Some(agent)).ok_or("invalid archive owner")?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| "archive directory unavailable")?;
    let root = home
        .canonicalize()
        .map_err(|_| "archive home unavailable")?;
    if dir
        .canonicalize()
        .map_err(|_| "archive directory unavailable")?
        != root.join("agents").join(agent).join("attachments")
    {
        return Err("archive outside home".into());
    }
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
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "archive clock invalid")?
        .as_millis();
    for increment in 0..8 {
        let name = format!("{}_{}", millis + increment, safe);
        store
            .bind_artifact_audience(
                &uuid::Uuid::new_v4().to_string(),
                task,
                agent,
                &name,
                audience,
            )
            .await?;
        let path = dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        match options.open(&path) {
            Ok(file) => {
                let mut file = tokio::fs::File::from_std(file);
                file.write_all(data)
                    .await
                    .map_err(|_| "private archive write failed")?;
                file.sync_all()
                    .await
                    .map_err(|_| "private archive sync failed")?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(_) => return Err("private archive create failed".into()),
        }
    }
    Err("private archive name collisions".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn private_archive_keeps_durable_audience_without_provenance_log() {
        let home = tempfile::tempdir().unwrap();
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
    /// V-M-1: a second name for a private archive (symbolic or hard link in
    /// the same attachments directory) is refused, not treated as unbound.
    #[cfg(unix)]
    #[tokio::test]
    async fn link_to_private_archive_under_new_name_is_refused() {
        let home = tempfile::tempdir().unwrap();
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
        let home = tempfile::tempdir().unwrap();
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

#[cfg(test)]
mod bound_history_tests {
    use super::*;
    #[tokio::test]
    async fn conversation_archives_require_trusted_own_stamp_but_keep_owner_full() {
        use duduclaw_auth::{UserDb,UserRole,AccessLevel};
        let home=tempfile::tempdir().unwrap();
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
