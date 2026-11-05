//! Shared admission for every human-origin cron writer, under BEGIN IMMEDIATE.
use super::*;
use cron::TimeUnitSpec;

pub(crate) const MAX_ENABLED_PER_USER: i64 = 20;
pub(crate) const MAX_ENABLED_PER_AGENT: i64 = 50;
pub(crate) const MAX_CREATIONS_PER_HOUR: i64 = 20;
pub const MAX_CRON_BYTES: usize = 256;
const MIN_CADENCE_SECONDS: u32 = 15 * 60;

/// Prove the minimum gap for every time-of-day slot, including midnight.
/// Calendar restrictions can only remove slots. Unlike sampling upcoming dates,
/// this cannot miss a high-frequency expression restricted to a future year.
/// Conservatively treats selected days as consecutive, even for weekly jobs.
pub(crate) fn validate_cadence(expression: &str) -> Result<(), String> {
    if expression.len() > MAX_CRON_BYTES { return Err("Automation schedule exceeds 256 bytes".into()); }
    let schedule = crate::cron_scheduler::normalise_cron(expression)
        .parse::<cron::Schedule>().map_err(|_| "Invalid automation schedule")?;
    let mut first = None;
    let mut previous = None;
    for hour in schedule.hours().iter() {
        for minute in schedule.minutes().iter() {
            for second in schedule.seconds().iter() {
                let slot = hour * 3600 + minute * 60 + second;
                if previous.is_some_and(|p| slot - p < MIN_CADENCE_SECONDS) {
                    return Err("Automation interval must be at least 15 minutes".into());
                }
                first.get_or_insert(slot);
                previous = Some(slot);
            }
        }
    }
    match (first, previous) {
        (Some(first), Some(last)) if 86400 + first - last >= MIN_CADENCE_SECONDS => Ok(()),
        _ => Err("Automation interval must be at least 15 minutes".into()),
    }
}

/// Scoped creation does not expose timezone selection. Refuse adopting zoned
/// legacy jobs: a DST forward jump can compress otherwise-valid clock slots
/// below 15 elapsed minutes. Admin retains the timezone-capable API.
pub(crate) fn scoped_cron_timezone_allowed(row: &CronTaskRow) -> bool {
    matches!(row.cron_timezone.as_deref(), None | Some("") | Some("UTC") | Some("Etc/UTC") | Some("Etc/GMT") | Some("GMT"))
}

pub(crate) fn validate_scoped_schedule(row: &CronTaskRow) -> Result<(), String> {
    if !scoped_cron_timezone_allowed(row) {
        return Err("Employee automations require UTC schedules; ask an administrator to manage timezones".into());
    }
    validate_cadence(&row.cron)
}

/// Presence, not truthiness: malformed/empty markers must never become Admin work.
pub(crate) fn requires_admission(row: &CronTaskRow) -> bool {
    row.on_behalf_of.is_some() || row.created_by_user_id.is_some() || row.scoped_by_user_id.is_some()
}

fn quota_users(row: &CronTaskRow) -> Result<Vec<String>, String> {
    let mut users = Vec::new();
    for user in [&row.created_by_user_id, &row.scoped_by_user_id].into_iter().flatten() {
        if duduclaw_core::HumanPrincipal::user(user).is_none() {
            return Err("Invalid automation actor".into());
        }
        users.push(user.clone());
    }
    if let Some(raw) = &row.on_behalf_of {
        let principal = duduclaw_core::HumanPrincipal::parse(raw)
            .map_err(|_| "Invalid automation principal")?;
        users.push(principal.user_id().map(str::to_owned).unwrap_or_else(|| principal.to_string()));
    }
    users.sort();
    users.dedup();
    Ok(users)
}

pub(super) fn check_write(conn: &Connection, row: &CronTaskRow) -> Result<(), String> {
    if !requires_admission(row) { return Ok(()); }
    validate_scoped_schedule(row)?;
    let users = quota_users(row)?;
    if !row.enabled { return Ok(()); }
    // Count both dashboard markers and MCP-only human origins. Exclude this
    // row so editing at the cap does not count the same job twice.
    for user in users {
        let principal = if user.starts_with("unmapped:") { user.clone() } else { format!("user:{user}") };
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM cron_tasks WHERE enabled=1 AND id<>?1
             AND (created_by_user_id=?2 OR scoped_by_user_id=?2 OR on_behalf_of=?3)",
            params![row.id, user, principal], |r| r.get(0)).map_err(|e| e.to_string())?;
        if count >= MAX_ENABLED_PER_USER { return Err("Enabled automation limit reached for user".into()); }
    }
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM cron_tasks WHERE enabled=1 AND id<>?1 AND agent_id=?2",
        params![row.id, row.agent_id], |r| r.get(0)).map_err(|e| e.to_string())?;
    if count >= MAX_ENABLED_PER_AGENT { return Err("Enabled automation limit reached for agent".into()); }
    Ok(())
}

/// Charge creation, or adoption by a new principal, in the same transaction.
/// Ordinary edits/resumes retain their existing receipt; deletion never refunds it.
pub(super) fn record_creation(conn: &Connection, row: &CronTaskRow, previous: Option<&CronTaskRow>) -> Result<(), String> {
    let previous_users = previous.map(quota_users).transpose()?.unwrap_or_default();
    let now = Utc::now().timestamp();
    conn.execute("DELETE FROM cron_creation_receipts WHERE created_unix<=?1", params![now - 3600])
        .map_err(|e| e.to_string())?;
    for user in quota_users(row)?.into_iter().filter(|user| !previous_users.contains(user)) {
        let count: i64 = conn.query_row("SELECT COUNT(*) FROM cron_creation_receipts WHERE user_id=?1",
            params![user], |r| r.get(0)).map_err(|e| e.to_string())?;
        if count >= MAX_CREATIONS_PER_HOUR { return Err("Automation creation limit reached (20 per hour)".into()); }
        conn.execute("INSERT INTO cron_creation_receipts(user_id,created_unix) VALUES(?1,?2)", params![user, now])
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub(super) fn check_transition(conn: &Connection, before: &CronTaskRow, after: &CronTaskRow) -> Result<(), String> {
    // An exact pause remains possible even for invalid legacy rows/over-quota users.
    let mut paused = before.clone();
    paused.enabled = false;
    if !after.enabled && serde_json::to_value(&paused).map_err(|e| e.to_string())?
        == serde_json::to_value(after).map_err(|e| e.to_string())? { return Ok(()); }
    check_write(conn, after)?;
    record_creation(conn, after, Some(before))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, user: &str, agent: &str) -> CronTaskRow {
        let mut row = CronTaskRow::new(id.into(), id.into(), agent.into(), "*/15 * * * *".into(), "report".into());
        row.created_by_user_id = Some(user.into());
        row.scoped_by_user_id = Some(user.into());
        row
    }

    #[test]
    fn scoped_cron_cadence_checks_all_slots_not_just_next_occurrences() {
        for expression in ["*/15 * * * *", "0 9 * * *", "0 0,15,30,45 * * * *", "0 0 1 * *"] {
            assert!(validate_cadence(expression).is_ok(), "{expression}");
        }
        for expression in ["* * * * *", "*/10 * * * *", "0,59 * * * *", "0 0,14 9 * * *",
            "0 0,59 0,23 * * *", "*/30 0 9 1 1 * 2090", "invalid"] {
            assert!(validate_cadence(expression).is_err(), "{expression}");
        }
    }

    #[tokio::test]
    async fn scoped_cron_creation_receipts_survive_delete_and_reopen() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        for i in 0..MAX_CREATIONS_PER_HOUR {
            let row = row(&format!("r{i}"), "user", "agent");
            store.insert_scoped(&row).await.unwrap();
            store.delete(&row.id).await.unwrap();
        }
        drop(store);
        let store = CronStore::open(home.path()).unwrap();
        assert!(store.insert_scoped(&row("overflow", "user", "agent")).await.unwrap_err().contains("per hour"));
        // Another user has an independent creation budget.
        store.insert_scoped(&row("other", "other", "agent")).await.unwrap();
        store.conn.lock().await.execute("UPDATE cron_creation_receipts SET created_unix=?1 WHERE user_id='user'",
            params![Utc::now().timestamp() - 3601]).unwrap();
        store.insert_scoped(&row("expired", "user", "agent")).await.unwrap();
    }

    #[tokio::test]
    async fn scoped_cron_caps_cover_resume_edit_and_shared_ownership() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        for i in 0..MAX_ENABLED_PER_USER {
            store.insert(&row(&format!("u{i}"), "owner", "agent")).await.unwrap();
        }
        store.conn.lock().await.execute("UPDATE cron_creation_receipts SET created_unix=0", []).unwrap();
        let mut pending = row("pending", "owner", "agent");
        pending.enabled = false;
        store.insert_scoped(&pending).await.unwrap();
        let mut enabled = pending.clone();
        enabled.enabled = true;
        enabled.scoped_by_user_id = Some("other-operator".into());
        assert!(store.edit_scoped(&pending, Some(&enabled)).await.is_err(), "creator cap survives a new operator");
        let original = store.get("u0").await.unwrap().unwrap();
        let mut edited = original.clone();
        edited.task = "updated".into();
        assert!(store.edit_scoped(&original, Some(&edited)).await.unwrap(), "edits at cap do not double count");
        let original = store.get("u0").await.unwrap().unwrap();
        let mut paused = original.clone();
        paused.enabled = false;
        assert!(store.edit_scoped(&original, Some(&paused)).await.unwrap());
        assert!(store.edit_scoped(&pending, Some(&enabled)).await.unwrap());
        // Last actor also gets a cap even when editing somebody else's job.
        for i in 0..MAX_ENABLED_PER_USER - 1 {
            store.insert(&row(&format!("a{i}"), "other-operator", "different-agent")).await.unwrap();
        }
        let foreign = row("foreign", "third", "agent");
        store.insert(&foreign).await.unwrap();
        let mut changed = foreign.clone();
        changed.scoped_by_user_id = Some("other-operator".into());
        assert!(store.edit_scoped(&foreign, Some(&changed)).await.is_err());
    }

    #[tokio::test]
    async fn scoped_cron_agent_cap_counts_admin_jobs_and_admin_insert_is_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        for i in 0..MAX_ENABLED_PER_AGENT {
            let mut admin = row(&format!("admin{i}"), "admin", "agent");
            admin.created_by_user_id = None;
            admin.scoped_by_user_id = None;
            store.insert(&admin).await.unwrap();
        }
        assert!(store.insert_scoped(&row("refused", "user", "agent")).await.unwrap_err().contains("for agent"));
        let mut admin = row("admin-over-cap", "admin", "agent");
        admin.created_by_user_id = None;
        admin.scoped_by_user_id = None;
        admin.cron = "* * * * * *".into();
        store.insert(&admin).await.unwrap();
        let mut disabled = row("disabled", "user", "agent");
        disabled.enabled = false;
        store.insert_scoped(&disabled).await.unwrap();
        let mut enabled = disabled.clone();
        enabled.enabled = true;
        assert!(store.edit_scoped(&disabled, Some(&enabled)).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scoped_cron_concurrent_connections_cannot_exceed_user_cap() {
        let home = tempfile::tempdir().unwrap();
        let first = std::sync::Arc::new(CronStore::open(home.path()).unwrap());
        let second = std::sync::Arc::new(CronStore::open(home.path()).unwrap());
        for i in 0..MAX_ENABLED_PER_USER - 1 {
            first.insert(&row(&format!("seed{i}"), "user", "agent")).await.unwrap();
        }
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let mut jobs = Vec::new();
        for (i, store) in [first.clone(), second].into_iter().enumerate() {
            let barrier = barrier.clone();
            jobs.push(tokio::spawn(async move {
                barrier.wait().await;
                store.insert_scoped(&row(&format!("race{i}"), "user", "agent")).await
            }));
        }
        let mut accepted = 0;
        for job in jobs { accepted += usize::from(job.await.unwrap().is_ok()); }
        assert_eq!(accepted, 1);
        assert_eq!(first.list_all().await.unwrap().len(), MAX_ENABLED_PER_USER as usize);
    }

    #[tokio::test]
    async fn scoped_cron_store_refuses_invalid_cadence_without_partial_write() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        let mut invalid = row("bad", "user", "agent");
        invalid.cron = "* * * * * *".into();
        assert!(store.insert_scoped(&invalid).await.is_err());
        assert!(store.get("bad").await.unwrap().is_none());
        let valid = row("valid", "user", "agent");
        store.insert_scoped(&valid).await.unwrap();
        let mut zoned = valid.clone();
        zoned.cron_timezone = Some("Australia/Lord_Howe".into());
        // 01:50 and 02:30 can be only 10 elapsed minutes apart at DST start.
        zoned.cron = "30,50 1,2 * * *".into();
        assert!(validate_scoped_schedule(&zoned).is_err());
        assert!(store.edit_scoped(&valid, Some(&zoned)).await.is_err());
        let mut changed = valid.clone();
        changed.cron = invalid.cron;
        assert!(store.edit_scoped(&valid, Some(&changed)).await.is_err());
        assert_eq!(store.get("valid").await.unwrap().unwrap().cron, valid.cron);
        let receipts: i64 = store.conn.lock().await.query_row("SELECT COUNT(*) FROM cron_creation_receipts", [], |r| r.get(0)).unwrap();
        assert_eq!(receipts, 1);
    }
    #[tokio::test]
    async fn shared_cron_admission_covers_each_marker_and_all_mutation_rails() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        for marker in ["principal", "creator", "actor"] {
            let mut task = CronTaskRow::new(marker.into(), marker.into(), "agent".into(), "* * * * *".into(), "work".into());
            match marker {
                "principal" => task.on_behalf_of = Some("user:human".into()),
                "creator" => task.created_by_user_id = Some("human".into()),
                _ => task.scoped_by_user_id = Some("human".into()),
            }
            assert!(store.insert(&task).await.is_err());
            task.cron = "*/15 * * * *".into();
            store.insert(&task).await.unwrap();
            assert!(store.update_fields(marker, marker, "agent", "* * * * *", "work", true).await.is_err());
            assert!(store.update_fields_with_notify(marker, marker, "agent", "* * * * *", "work", true,
                Some("line"), Some("target"), None).await.is_err());
            assert!(store.update_fields_with_principal(marker, marker, "agent", "* * * * *", "work", true,
                None, None).await.is_err());
            assert!(store.update_cron_timezone(marker, Some("America/New_York")).await.is_err());
            let saved = store.get(marker).await.unwrap().unwrap();
            assert_eq!(saved.cron, task.cron);
            assert!(saved.notify_channel.is_none());
            // Simulate a row left behind by an older writer. All resume paths
            // refuse it, while an exact pause remains available.
            store.conn.lock().await.execute("UPDATE cron_tasks SET cron='* * * * *' WHERE id=?1", params![marker]).unwrap();
            store.set_enabled(marker, false).await.unwrap();
            assert!(store.set_enabled(marker, true).await.is_err());
            assert!(store.set_enabled_with_principal(marker, true, Some("user:human")).await.is_err());
            assert!(store.set_enabled_by_name(marker, true).await.is_err());
            assert!(!store.get(marker).await.unwrap().unwrap().enabled);
        }
        let count: i64 = store.conn.lock().await.query_row("SELECT COUNT(*) FROM cron_creation_receipts", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 3, "failed writes and ordinary edits do not charge receipts");
    }

    #[tokio::test]
    async fn shared_cron_mcp_principals_share_dashboard_caps_and_durable_rate_limit() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        for i in 0..MAX_ENABLED_PER_USER {
            let mut task = row(&format!("mcp{i}"), "user", "agent");
            task.created_by_user_id = None; task.scoped_by_user_id = None;
            task.on_behalf_of = Some("user:user".into());
            store.insert(&task).await.unwrap();
        }
        assert!(store.insert_scoped(&row("dashboard", "user", "agent")).await.unwrap_err().contains("for user"));
        let mut legacy = CronTaskRow::new("legacy".into(), "legacy".into(), "agent".into(), "0 9 * * *".into(), "work".into());
        legacy.enabled = false;
        store.insert(&legacy).await.unwrap();
        assert!(store.set_enabled_with_principal("legacy", true, Some("user:user")).await.unwrap_err().contains("for user"));
        assert!(store.get("legacy").await.unwrap().unwrap().on_behalf_of.is_none());
        for i in 0..MAX_ENABLED_PER_USER { store.delete(&format!("mcp{i}")).await.unwrap(); }
        drop(store);
        let store = CronStore::open(home.path()).unwrap();
        assert!(store.insert_scoped(&row("dashboard", "user", "agent")).await.unwrap_err().contains("per hour"));
        assert!(store.set_enabled_with_principal("legacy", true, Some("user:user")).await.unwrap_err().contains("per hour"));
        assert!(store.get("legacy").await.unwrap().unwrap().on_behalf_of.is_none());
    }

    #[tokio::test]
    async fn shared_cron_agent_move_and_bulk_resume_roll_back_atomically() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        for i in 0..MAX_ENABLED_PER_AGENT {
            store.insert(&CronTaskRow::new(format!("admin{i}"), "admin".into(), "full".into(), "* * * * *".into(), "work".into())).await.unwrap();
        }
        let task = row("move", "user", "empty");
        store.insert(&task).await.unwrap();
        assert!(store.update_fields("move", "move", "full", &task.cron, "work", true).await.unwrap_err().contains("for agent"));
        assert_eq!(store.get("move").await.unwrap().unwrap().agent_id, "empty");
        for i in 0..2 {
            let mut task = row(&format!("bulk{i}"), "user", "full");
            task.name = "bulk".into(); task.enabled = false;
            store.insert(&task).await.unwrap();
        }
        store.delete("admin0").await.unwrap();
        assert!(store.set_enabled_by_name("bulk", true).await.is_err());
        assert!(!store.get("bulk0").await.unwrap().unwrap().enabled);
        assert!(!store.get("bulk1").await.unwrap().unwrap().enabled);
    }

    #[tokio::test]
    async fn shared_cron_unmapped_and_malformed_origins_do_not_become_admin() {
        let home = tempfile::tempdir().unwrap();
        let store = CronStore::open(home.path()).unwrap();
        let mut task = CronTaskRow::new("origin".into(), "origin".into(), "agent".into(), "0 9 * * *".into(), "work".into());
        for principal in ["", "unknown:rejected-principal", "user:"] {
            task.on_behalf_of = Some(principal.into());
            assert!(store.insert(&task).await.is_err());
        }
        task.on_behalf_of = Some("unmapped:line:0123456789abcdef".into());
        store.insert(&task).await.unwrap();
        assert!(store.update_fields("origin", "origin", "agent", "* * * * *", "work", true).await.is_err());
    }

}
