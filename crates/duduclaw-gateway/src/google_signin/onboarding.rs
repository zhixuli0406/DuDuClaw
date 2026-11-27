//! Single-gateway maintenance for Google-provisioned members.
use std::{path::Path, sync::Arc};
use duduclaw_auth::{signin_onboarding::SigninAvatarJob, UserDb};
use rand::Rng;
use super::{avatar::{SigninAvatarError, SigninAvatarRequest}, config};
use crate::handlers::MethodHandler;

pub(crate) fn should_run(home: &Path) -> bool {
    duduclaw_core::gateway_instance::held(home)
}

pub(super) fn avatar_name(email: &str) -> String {
    let local = email.split('@').next().unwrap_or("");
    let mut slug = String::new();
    for c in local.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = duduclaw_core::truncate_chars(slug.trim_matches('-'), 40);
    let slug = slug.trim_matches('-');
    let slug = if slug.is_empty() { "member" } else { slug };
    let alphabet = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let suffix: String = (0..4).map(|_| {
        char::from(alphabet[rand::thread_rng().gen_range(0..alphabet.len())])
    }).collect();
    format!("avatar-{slug}-{suffix}")
}

pub(super) fn avatar_display_name(name: &str) -> String {
    let clean: String = name.chars().filter(|c| !c.is_control()).collect();
    duduclaw_core::truncate_chars(&format!("{clean} 的 AI 分身"), 200)
}

async fn db_call<T: Send + 'static>(db: &Arc<UserDb>,
    f: impl FnOnce(&UserDb) -> Result<T, String> + Send + 'static) -> Result<T, ()> {
    let db = db.clone();
    tokio::task::spawn_blocking(move || f(&db)).await.map_err(|_| ())?.map_err(|_| ())
}

/// Caller must hold the gateway lock. Kept separate from the timer for tests.
pub(crate) async fn run_once(handler: &MethodHandler, now: i64) {
    let Some(db) = handler.signin_onboarding_db().await else {
        tracing::warn!(outcome = "database_unavailable", "signin avatar maintenance");
        return;
    };
    if db_call(&db, move |db| db.expire_signin_requests(now)).await.is_err() {
        tracing::warn!(outcome = "expiry_failed", "signin avatar maintenance");
        return;
    }
    let home = handler.home_dir().to_path_buf();
    let settings = match tokio::task::spawn_blocking(move || config::read_strict(&home)).await {
        Ok(settings) if settings.enabled => settings,
        Ok(_) => return,
        Err(_) => {
            tracing::warn!(outcome = "settings_unavailable", "signin avatar maintenance");
            return;
        }
    };
    let jobs = match db_call(&db, move |db| db.signin_avatar_due(now, 20)).await {
        Ok(jobs) => jobs,
        Err(()) => {
            tracing::warn!(outcome = "due_failed", "signin avatar maintenance");
            return;
        }
    };
    for job in jobs {
        let user_id = job.user_id.clone();
        if process_job(handler, &db, &settings, job, now).await.is_err() {
            tracing::warn!(user_id = %user_id, outcome = "job_failed", "signin avatar maintenance");
        }
    }
}

async fn process_job(handler: &MethodHandler, db: &Arc<UserDb>, settings: &config::GoogleSigninSettings,
    job: SigninAvatarJob, now: i64) -> Result<(), ()> {
    let id = job.user_id.clone();
    if db_call(db, move |db| db.avatar_agent(&id)).await?.is_some() {
        let id = job.user_id.clone();
        db_call(db, move |db| db.signin_avatar_mark_done_if_present(&id)).await?;
        tracing::info!(user_id = %job.user_id, outcome = "done", "signin avatar maintenance");
        return Ok(());
    }

    let mut state = job.avatar_state.as_str();
    if state == "creating" {
        let agents_dir = handler.registry().read().await.agents_dir().to_path_buf();
        let old_name = job.avatar_agent_name.clone();
        let orphan = tokio::task::spawn_blocking(move || -> Result<Option<String>, ()> {
            let Some(name) = old_name else { return Ok(None); };
            // Treat stored names as untrusted, even though reservations validate them.
            if !duduclaw_core::is_valid_agent_id(&name) { return Err(()); }
            match std::fs::symlink_metadata(agents_dir.join(&name)) {
                Ok(_) => Ok(Some(format!("orphan:{name}"))),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(_) => Err(()),
            }
        }).await.map_err(|_| ())??;
        let id = job.user_id.clone();
        let found_orphan = orphan.is_some();
        db_call(db, move |db| db.signin_avatar_requeue(&id, orphan.as_deref())).await?;
        if found_orphan {
            tracing::warn!(user_id = %job.user_id, outcome = "orphan", "signin avatar maintenance");
        }
        state = "pending";
    }

    let name = if matches!(state, "pending" | "retry") {
        // Never reuse the interrupted attempt's name, even on a random collision.
        let name = (0..20).map(|_| avatar_name(&job.email))
            .find(|name| job.avatar_agent_name.as_ref() != Some(name)).ok_or(())?;
        if !duduclaw_core::is_valid_agent_id(&name) { return Err(()); }
        let id = job.user_id.clone();
        let reserved = name.clone();
        db_call(db, move |db| db.signin_avatar_reserve(&id, &reserved)).await?;
        name
    } else if state == "reserved" {
        job.avatar_agent_name.clone().filter(|name| duduclaw_core::is_valid_agent_id(name)).ok_or(())?
    } else {
        return Err(());
    };
    let id = job.user_id.clone();
    let creating = name.clone();
    db_call(db, move |db| db.signin_avatar_creating(&id, &creating)).await?;
    let result = handler.create_signin_avatar(SigninAvatarRequest {
        member_id: job.user_id.clone(), agent_name: name,
        display_name: avatar_display_name(&job.display_name),
        template: settings.avatar_template.clone(), settings_revision: settings.revision.clone(),
    }).await;
    let id = job.user_id.clone();
    let outcome = match result {
        Ok(()) => {
            db_call(db, move |db| db.signin_avatar_mark_done_if_present(&id)).await?;
            "done"
        }
        Err(SigninAvatarError::NameTaken | SigninAvatarError::PolicyChanged) => {
            db_call(db, move |db| db.signin_avatar_requeue(&id, None)).await?;
            if result == Err(SigninAvatarError::NameTaken) { "name_taken" } else { "policy_changed" }
        }
        Err(SigninAvatarError::Failed) => {
            let delay = match job.attempts { 0 => 60, 1 => 600, _ => 3600 };
            db_call(db, move |db| db.signin_avatar_retry(&id, now.saturating_add(delay), "failed")).await?;
            "failed"
        }
        Err(error) => {
            let reason = match error {
                SigninAvatarError::ProtectedMode => "protected_mode",
                SigninAvatarError::TemplateChanged => "template_changed",
                SigninAvatarError::TemplateInvalid => "template_invalid",
                SigninAvatarError::SeatsFull => "seats_full",
                SigninAvatarError::NotEligible => "not_eligible",
                // The three retryable outcomes were handled above.
                _ => return Err(()),
            };
            db_call(db, move |db| db.signin_avatar_wait_admin(&id, reason)).await?;
            reason
        }
    };
    if outcome == "done" {
        tracing::info!(user_id = %job.user_id, outcome, "signin avatar maintenance");
    } else {
        tracing::warn!(user_id = %job.user_id, outcome, "signin avatar maintenance");
    }
    Ok(())
}
