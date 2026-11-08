//! Stable execution contracts. Runtime counters never renew an approval, while
//! edits to the work, owner, recipient or target always change its material.
use serde_json::{json, Value};

pub fn cron(row: &crate::cron_store::CronTaskRow) -> Value {
    json!({"id": row.id, "name": row.name, "agent_id": row.agent_id,
        "cron": row.cron, "task": row.task, "cron_timezone": row.cron_timezone,
        "trigger_kind": row.trigger_kind, "condition_script": row.condition_script,
        "watch_command": row.watch_command, "notify_channel": row.notify_channel,
        "notify_chat_id": row.notify_chat_id, "notify_thread_id": row.notify_thread_id,
        "on_behalf_of": row.on_behalf_of, "created_at": row.created_at,
        "created_by_user_id": row.created_by_user_id, "scoped_by_user_id": row.scoped_by_user_id})
}

pub fn reminder(row: &crate::reminder_scheduler::Reminder) -> Value {
    json!({"id": row.id, "agent_id": row.agent_id, "trigger_at": row.trigger_at,
        "channel": row.channel, "chat_id": row.chat_id, "message": row.message,
        "prompt": row.prompt, "mode": row.mode, "sender_agent": row.sender_agent,
        "on_behalf_of": row.on_behalf_of, "created_at": row.created_at})
}

pub fn task(row: &crate::task_store::TaskRow) -> Value {
    json!({"id": row.id, "title": row.title, "description": row.description,
        "assigned_to": row.assigned_to, "created_by": row.created_by, "created_at": row.created_at,
        "parent_task_id": row.parent_task_id, "depends_on": row.depends_on,
        "max_retries": row.max_retries, "goal_mode": row.goal_mode,
        "acceptance_criteria": row.acceptance_criteria,
        "acceptance_criteria_baseline": row.acceptance_criteria_baseline,
        "goal_id": row.goal_id, "deadline_at": row.deadline_at, "cost_cap_cents": row.cost_cap_cents,
        "timeout_secs": row.timeout_secs, "risk_boundary": row.risk_boundary,
        "team_spec_json": row.team_spec_json, "kind": row.kind,
        "discovery_spec_json": row.discovery_spec_json, "discovery_approval_id": row.discovery_approval_id,
        "source_channel": row.source_channel, "source_chat_id": row.source_chat_id,
        "source_discord_guild_id": row.source_discord_guild_id,
        "on_behalf_of": row.on_behalf_of, "created_via": row.created_via})
}

/// This is the same snapshot handed to the gateway callback by the heartbeat
/// scheduler. Daily observation text is data, not part of the recurring grant.
pub fn heartbeat(home: &std::path::Path, agent: &str) -> Result<Value, String> {
    if !duduclaw_core::is_valid_agent_id(agent) { return Err("invalid heartbeat employee".into()); }
    let dir = home.join("agents").join(agent);
    let content = read_bounded(&dir.join("agent.toml"), 1024 * 1024)?;
    let config: duduclaw_core::types::AgentConfig = toml::from_str(&content).map_err(|e| e.to_string())?;
    // The scheduler caps this input at 64 KiB. Reject oversize input before its
    // legacy truncation path so review and execution cannot bind different text.
    let checks = match read_bounded(&dir.join("PROACTIVE.md"), 64 * 1024) {
        Ok(checks) if !checks.trim().is_empty() => Some(checks),
        Ok(_) => None,
        Err(error) if dir.join("PROACTIVE.md").exists() => return Err(error),
        Err(_) => None,
    };
    let checks = match checks {
        Some(checks) => checks,
        None if config.capabilities.os_native => duduclaw_agent::proactive::DEFAULT_OS_CARE_CHECKS.into(),
        None => return Err("heartbeat checks unavailable".into()),
    };
    Ok(json!({"agent_id": agent, "proactive": config.proactive, "checks": checks}))
}

pub fn task_spec(spec: &crate::task_spec::TaskSpec, step_index: usize) -> Value {
    let steps: Vec<_> = spec.steps.iter().map(|step| json!({"id": step.id,
        "description": step.description, "agent": step.agent,
        "depends_on": step.depends_on, "acceptance_criteria": step.acceptance_criteria})).collect();
    json!({"task_id": spec.task_id, "agent_id": spec.agent_id, "goal": spec.goal,
        "on_behalf_of": spec.on_behalf_of, "max_replans": spec.max_replans,
        "created_at": spec.created_at, "steps": steps, "step_index": step_index})
}

fn read_bounded(path: &std::path::Path, limit: u64) -> Result<String, String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() { return Err("source is not a file".into()); }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > limit { return Err("background source exceeds read limit".into()); }
    String::from_utf8(bytes).map_err(|_| "background source is not UTF-8".into())
}

fn read_db(home: &std::path::Path, name: &str) -> Result<rusqlite::Connection, String> {
    let conn = rusqlite::Connection::open_with_flags(home.join(name),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_millis(100)).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
        .map_err(|e| e.to_string())?;
    Ok(conn)
}

/// Revalidation reads the authoritative record, not the cached schedule or the
/// approval's copy. A missing, malformed, terminal or edited source revokes.
pub fn read_current(home: &std::path::Path, kind: &str, id: &str) -> Result<Value, String> {
    read_source(home, kind, id, false)
}

/// A completed parent may have already delegated durable work. Its completion
/// does not cancel descendants; cancellation, failure or changed material does.
pub fn read_ancestor(home: &std::path::Path, kind: &str, id: &str) -> Result<Value, String> {
    read_source(home, kind, id, true)
}

fn read_source(home: &std::path::Path, kind: &str, id: &str, ancestor: bool) -> Result<Value, String> {
    if id.is_empty() || id.len() > 512 { return Err("invalid background record".into()); }
    match kind {
        "cron" => {
            let conn = read_db(home, "cron_tasks.db")?;
            let row = conn.query_row("SELECT id,name,agent_id,cron,task,enabled,
                created_at,updated_at,last_run_at,last_status,last_error,run_count,failure_count,
                notify_channel,notify_chat_id,notify_thread_id,cron_timezone,trigger_kind,
                condition_script,condition_state,watch_command,on_behalf_of,created_by_user_id,scoped_by_user_id
                FROM cron_tasks WHERE id=?1", [id], crate::cron_store::row_to_cron_task)
                .map_err(|e| e.to_string())?;
            if !row.enabled { return Err("background cron is disabled".into()); }
            Ok(cron(&row))
        }
        "task" => {
            let conn = read_db(home, "tasks.db")?;
            let row = conn.query_row(&format!("SELECT {} FROM tasks WHERE id=?1", crate::task_store::TASK_COLUMNS),
                [id], crate::task_store::row_to_task).map_err(|e| e.to_string())?;
            if row.archived || matches!(row.status.as_str(), "cancelled" | "failed")
                || (!ancestor && row.status == "done") {
                return Err("background task is terminal".into());
            }
            Ok(task(&row))
        }
        "reminder" => {
            let content = read_bounded(&home.join("reminders.jsonl"), 8 * 1024 * 1024)?;
            let mut selected = None;
            for line in content.lines().filter(|line| !line.trim().is_empty()) {
                let row: crate::reminder_scheduler::Reminder = serde_json::from_str(line).map_err(|e| e.to_string())?;
                if row.id == id {
                    let allowed_status = row.status == crate::reminder_scheduler::ReminderStatus::Pending
                        || (ancestor && row.status == crate::reminder_scheduler::ReminderStatus::Delivered);
                    if selected.is_some() || !allowed_status {
                        return Err("background reminder is ambiguous or terminal".into());
                    }
                    selected = Some(reminder(&row));
                }
            }
            selected.ok_or_else(|| "background reminder missing".into())
        }
        "heartbeat" => heartbeat(home, id),
        "task_spec" => {
            let parts: Vec<_> = id.split(':').collect();
            let [agent, task_id, "step", index] = parts.as_slice() else {
                return Err("invalid background task step".into());
            };
            if !duduclaw_core::is_valid_agent_id(agent) || task_id.is_empty()
                || !task_id.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') {
                return Err("invalid background task step".into());
            }
            let index: usize = index.parse().map_err(|_| "invalid background step index".to_string())?;
            let path = home.join("agents").join(agent).join("tasks").join(format!("{task_id}.json"));
            let spec: crate::task_spec::TaskSpec = serde_json::from_str(&read_bounded(&path, 8 * 1024 * 1024)?)
                .map_err(|e| e.to_string())?;
            if spec.agent_id != *agent || spec.task_id != *task_id || index >= spec.steps.len() {
                return Err("background task step binding mismatch".into());
            }
            if matches!(spec.status, crate::task_spec::TaskStatus::Failed | crate::task_spec::TaskStatus::Cancelled)
                || (!ancestor && spec.status == crate::task_spec::TaskStatus::Completed) {
                return Err("background task step is terminal".into());
            }
            let step = &spec.steps[index];
            if matches!(step.status, crate::task_spec::StepStatus::Failed | crate::task_spec::StepStatus::Skipped)
                || (!ancestor && step.status == crate::task_spec::StepStatus::Passed) {
                return Err("background task step is terminal".into());
            }
            Ok(task_spec(&spec, index))
        }
        "delegation" => {
            let conn = read_db(home, "message_queue.db")?;
            conn.query_row("SELECT sender,target,payload,turn_id,session_id,lane,delegation_depth,
                origin_agent,reply_channel,sender_agent,upstream_unknown,on_behalf_of FROM message_queue
                WHERE id=?1 AND (status IN ('pending','acked','processing') OR (?2 AND status='done'))",
                rusqlite::params![id, ancestor], |row| {
                    let depth: i64 = row.get(6)?;
                    let depth = u8::try_from(depth).map_err(|error| rusqlite::Error::FromSqlConversionFailure(
                        6, rusqlite::types::Type::Integer, Box::new(error)))?;
                    Ok(delegation(&row.get::<_, String>(0)?, &row.get::<_, String>(1)?, &row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?.as_deref(), row.get::<_, Option<String>>(4)?.as_deref(),
                        row.get::<_, Option<String>>(5)?.as_deref(), depth, row.get::<_, Option<String>>(7)?.as_deref(),
                        row.get::<_, Option<String>>(8)?.as_deref(), row.get::<_, Option<String>>(9)?.as_deref(),
                        row.get::<_, bool>(10)?, 0, false, row.get::<_, Option<String>>(11)?.as_deref()))
                }).map_err(|e| e.to_string())
        }
        _ => Err("unsupported background source".into()),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn delegation(sender: &str, target: &str, payload: &str, turn: Option<&str>,
    session: Option<&str>, lane: Option<&str>, depth: u8, origin: Option<&str>,
    reply_channel: Option<&str>, sender_agent: Option<&str>, upstream_unknown: bool, hop_depth: u8, spawned: bool, owner: Option<&str>) -> Value {
    json!({"sender": sender, "target": target, "payload": payload, "turn": turn,
        "session": session, "lane": lane, "depth": depth, "origin": origin,
        "reply_channel": reply_channel, "sender_agent": sender_agent,
        "upstream_unknown": upstream_unknown, "hop_depth": hop_depth, "spawned": spawned, "on_behalf_of": owner})
}

/// SR2 round 3 (N2): the fields of a [`delegation`] that a Personal owner's
/// approval binds and that the resend after the approval ("delegate the same
/// work again") must repeat exactly. Turn and session ids (and the hop
/// bookkeeping) change on every resend, so they are not part of it.
pub const DELEGATION_RESEND_FIELDS: [&str; 6] = ["target", "payload", "reply_channel", "lane", "on_behalf_of", "spawned"];

/// The [`DELEGATION_RESEND_FIELDS`] of a delegation material. `None` when
/// any of them is missing (a material not built by [`delegation`]).
pub fn delegation_resend_binding(material: &Value) -> Option<Value> {
    let object = material.as_object()?;
    let mut bound = serde_json::Map::new();
    for name in DELEGATION_RESEND_FIELDS {
        bound.insert(name.to_owned(), object.get(name)?.clone());
    }
    Some(Value::Object(bound))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn background_cron_material_ignores_counters_but_binds_work_and_recipient() {
        let mut row = crate::cron_store::CronTaskRow::new("id".into(), "name".into(),
            "alice".into(), "0 0 * * *".into(), "original".into());
        let original = cron(&row);
        row.run_count = 17;
        row.last_run_at = Some("later".into());
        row.condition_state = Some("{\"observation\":2}".into());
        assert_eq!(original, cron(&row));
        row.task = "different".into();
        assert_ne!(original, cron(&row));
        row.task = "original".into();
        row.notify_chat_id = Some("new-recipient".into());
        assert_ne!(original, cron(&row));
    }

    #[test]
    fn background_task_material_survives_rounds_but_not_reassignment_or_contract_edit() {
        let mut row = crate::task_store::TaskRow::new("task".into(), "title".into(),
            "work".into(), "normal".into(), "alice".into(), "owner".into());
        let original = task(&row);
        row.revision_round += 1;
        row.authority_revision += 1;
        row.claimed_by = Some("alice".into());
        row.status = "in_progress".into();
        assert_eq!(original, task(&row));
        row.assigned_to = "bob".into();
        assert_ne!(original, task(&row));
        row.assigned_to = "alice".into();
        row.cost_cap_cents = Some(1000);
        assert_ne!(original, task(&row));
    }

    #[test]
    fn background_delegation_material_binds_original_turn_and_sender() {
        let original = delegation("alice", "bob", "work", Some("turn"), Some("session"), None, 1, Some("alice"), Some("line:room"), Some("alice"), false, 0, false, Some("user:owner"));
        assert_ne!(original, delegation("mallory", "bob", "work", Some("turn"), Some("session"), None, 1, Some("alice"), Some("line:room"), Some("alice"), false, 0, false, Some("user:owner")));
        assert_ne!(original, delegation("alice", "bob", "work", Some("other"), Some("session"), None, 1, Some("alice"), Some("line:room"), Some("alice"), false, 0, false, Some("user:owner")));
        assert_ne!(original, delegation("alice", "bob", "work", Some("turn"), Some("session"), Some("explore"), 1, Some("alice"), Some("line:room"), Some("alice"), false, 0, false, Some("user:owner")));
    }

    #[test]
    fn delegation_resend_binding_ignores_turn_and_session_only() {
        let bound = |turn: &str, session: &str, reply: &str, lane: Option<&str>, owner: &str, spawned: bool, payload: &str|
            delegation_resend_binding(&delegation("alice", "bob", payload, Some(turn), Some(session), lane, 1,
                Some("alice"), Some(reply), Some("alice"), false, 0, spawned, Some(owner))).unwrap();
        let original = bound("turn", "session", "line:room", None, "user:owner", false, "work");
        assert_eq!(original.as_object().unwrap().len(), DELEGATION_RESEND_FIELDS.len());
        assert_eq!(original, bound("turn-2", "session-2", "line:room", None, "user:owner", false, "work"));
        for changed in [bound("turn", "session", "line:other", None, "user:owner", false, "work"),
            bound("turn", "session", "line:room", Some("explore"), "user:owner", false, "work"),
            bound("turn", "session", "line:room", None, "user:other", false, "work"),
            bound("turn", "session", "line:room", None, "user:owner", true, "work"),
            bound("turn", "session", "line:room", None, "user:owner", false, "other work")] {
            assert_ne!(original, changed);
        }
        assert!(delegation_resend_binding(&json!({"target": "bob"})).is_none());
    }

    #[test]
    fn background_readback_never_creates_a_missing_database_or_accepts_path_traversal() {
        let home = tempfile::tempdir().unwrap();
        assert!(read_current(home.path(), "cron", "missing").is_err());
        assert!(!home.path().join("cron_tasks.db").exists());
        assert!(read_current(home.path(), "task", "missing").is_err());
        assert!(!home.path().join("tasks.db").exists());
        assert!(read_current(home.path(), "task_spec", "alice:../secret:step:0").is_err());
        assert!(read_current(home.path(), "task_spec", "../alice:task:step:0").is_err());
        assert!(read_current(home.path(), "unknown", "id").is_err());
    }

    #[test]
    fn background_live_cron_readback_observes_edits_and_disarm() {
        let home = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(home.path().join("cron_tasks.db")).unwrap();
        conn.execute_batch("CREATE TABLE cron_tasks(id TEXT,name TEXT,agent_id TEXT,cron TEXT,task TEXT,
            enabled INTEGER,created_at TEXT,updated_at TEXT,last_run_at TEXT,last_status TEXT,last_error TEXT,
            run_count INTEGER,failure_count INTEGER,notify_channel TEXT,notify_chat_id TEXT,notify_thread_id TEXT,
            cron_timezone TEXT,trigger_kind TEXT,condition_script TEXT,condition_state TEXT,watch_command TEXT,on_behalf_of TEXT,
            created_by_user_id TEXT,scoped_by_user_id TEXT);
            INSERT INTO cron_tasks VALUES('id','name','alice','0 0 * * *','work',1,'created','updated',
                NULL,NULL,NULL,0,0,NULL,NULL,NULL,NULL,'time',NULL,NULL,NULL,'user:owner','creator','creator');").unwrap();
        let original = read_current(home.path(), "cron", "id").unwrap();
        conn.execute("UPDATE cron_tasks SET run_count=17,last_status='ok'", []).unwrap();
        assert_eq!(original, read_current(home.path(), "cron", "id").unwrap());
        conn.execute("UPDATE cron_tasks SET scoped_by_user_id='other-employee'", []).unwrap();
        assert_ne!(original, read_current(home.path(), "cron", "id").unwrap());
        conn.execute("UPDATE cron_tasks SET scoped_by_user_id='creator',task='different work'", []).unwrap();
        assert_ne!(original, read_current(home.path(), "cron", "id").unwrap());
        conn.execute("UPDATE cron_tasks SET enabled=0", []).unwrap();
        assert!(read_current(home.path(), "cron", "id").is_err());
        assert!(read_ancestor(home.path(), "cron", "id").is_err());
    }

    #[test]
    fn background_live_delegation_readback_detects_edits_and_terminal_state() {
        let home = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open(home.path().join("message_queue.db")).unwrap();
        conn.execute_batch("CREATE TABLE message_queue(id TEXT, sender TEXT, target TEXT,
            payload TEXT,turn_id TEXT,session_id TEXT,lane TEXT,delegation_depth INTEGER,
            origin_agent TEXT,reply_channel TEXT,sender_agent TEXT,upstream_unknown INTEGER,
            on_behalf_of TEXT,status TEXT);
            INSERT INTO message_queue VALUES('message','alice','bob','work','turn','session',NULL,
                1,'alice','line:room','alice',0,'user:owner','pending');").unwrap();
        let original = delegation("alice", "bob", "work", Some("turn"), Some("session"), None,
            1, Some("alice"), Some("line:room"), Some("alice"), false, 0, false, Some("user:owner"));
        assert_eq!(original, read_current(home.path(), "delegation", "message").unwrap());
        conn.execute("UPDATE message_queue SET reply_channel='line:other'", []).unwrap();
        assert_ne!(original, read_current(home.path(), "delegation", "message").unwrap());
        conn.execute("UPDATE message_queue SET status='done'", []).unwrap();
        assert!(read_current(home.path(), "delegation", "message").is_err());
        assert!(read_ancestor(home.path(), "delegation", "message").is_ok());
        conn.execute("UPDATE message_queue SET status='failed'", []).unwrap();
        assert!(read_ancestor(home.path(), "delegation", "message").is_err());
    }

    #[test]
    fn background_reminder_readback_rejects_duplicate_malformed_or_cancelled_records() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("reminders.jsonl");
        let mut value = json!({"id":"reminder", "agent_id":"alice", "trigger_at":"2026-11-06T00:00:00Z",
            "channel":"line", "chat_id":"room", "mode":"agent_callback", "status":"pending",
            "prompt":"work", "on_behalf_of":"user:owner"});
        let row: crate::reminder_scheduler::Reminder = serde_json::from_value(value.clone()).unwrap();
        std::fs::write(&path, value.to_string()).unwrap();
        assert_eq!(reminder(&row), read_current(home.path(), "reminder", "reminder").unwrap());
        std::fs::write(&path, format!("{value}\n{value}\n")).unwrap();
        assert!(read_current(home.path(), "reminder", "reminder").is_err());
        std::fs::write(&path, format!("{value}\ninvalid\n")).unwrap();
        assert!(read_current(home.path(), "reminder", "reminder").is_err());
        value["status"] = json!("cancelled");
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(read_current(home.path(), "reminder", "reminder").is_err());
        assert!(read_ancestor(home.path(), "reminder", "reminder").is_err());
        value["status"] = json!("delivered");
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(read_current(home.path(), "reminder", "reminder").is_err());
        assert!(read_ancestor(home.path(), "reminder", "reminder").is_ok());
    }

    #[test]
    fn background_task_step_completion_allows_only_descendants_and_cancellation_revokes() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("agents/alice/tasks");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("task-id.json");
        let mut value = json!({"task_id":"task-id", "agent_id":"alice", "on_behalf_of":"user:owner",
            "goal":"work", "steps":[{"id":0,"description":"step","agent":"alice",
                "depends_on":[],"acceptance_criteria":[],"status":"running","result":null,"retry_count":0}],
            "current_step":0,"max_replans":2,"replan_count":0,"state":{},"artifacts":[],
            "status":"running","created_at":"2026-11-05T00:00:00Z","updated_at":"2026-11-05T00:00:00Z"});
        std::fs::write(&path, value.to_string()).unwrap();
        let original = read_current(home.path(), "task_spec", "alice:task-id:step:0").unwrap();
        value["steps"][0]["status"] = json!("passed");
        value["status"] = json!("completed");
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(read_current(home.path(), "task_spec", "alice:task-id:step:0").is_err());
        assert_eq!(original, read_ancestor(home.path(), "task_spec", "alice:task-id:step:0").unwrap());
        value["status"] = json!("cancelled");
        std::fs::write(&path, value.to_string()).unwrap();
        assert!(read_ancestor(home.path(), "task_spec", "alice:task-id:step:0").is_err());
    }
}
