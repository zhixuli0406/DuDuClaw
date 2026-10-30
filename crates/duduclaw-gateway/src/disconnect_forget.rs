//! Disconnect batches: one Admin card, leased execution, and receipts on success.

use std::path::Path;
use chrono::{Duration, Utc};
use duduclaw_memory::{ApplyOutcome, ForgetPlan, PlanOptions, PlanOutcome, SqliteMemoryEngine};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use crate::approval::operator_cli_gate::{self as gate, Binding, Filing, StatePolicy};
use crate::approval::ApprovalBroker;

const TTL_MINUTES: i64 = 7 * 24 * 60;
const ACTION: &str = "disconnect_forget";
const LEASE_SECONDS: i64 = 300;
const NOT_COVERED: &[&str] = &["conversation text", "tool_calls.jsonl", "wiki", "backups"];

fn connection(home: &Path) -> Result<Connection, String> {
    let conn = Connection::open_with_flags(home.join("memory.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
        .map_err(|e| e.to_string())?;
    conn.busy_timeout(std::time::Duration::from_secs(5)).map_err(|e| e.to_string())?;
    Ok(conn)
}

fn engine(home: &Path) -> Result<SqliteMemoryEngine, String> {
    if !home.join("memory.db").is_file() { return Err("memory.db is unavailable".into()); }
    SqliteMemoryEngine::new(&home.join("memory.db")).map_err(|e| e.to_string())
}

fn namespaces(home: &Path) -> Result<Vec<String>, String> {
    let conn = connection(home)?;
    let mut stmt = conn.prepare("SELECT agent_id FROM memories UNION SELECT agent_id FROM key_facts UNION SELECT agent_id FROM memory_origins ORDER BY 1")
        .map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
    let mut agents = rows.collect::<Result<Vec<String>, _>>().map_err(|e| e.to_string())?;
    let has_archive: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name='memories_archive')",
        [], |r| r.get(0)).map_err(|e| e.to_string())?;
    if has_archive {
        let mut archived = conn.prepare("SELECT DISTINCT agent_id FROM memories_archive").map_err(|e| e.to_string())?;
        let rows = archived.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
        agents.extend(rows.collect::<Result<Vec<String>, _>>().map_err(|e| e.to_string())?);
    }
    // An employee may have an in-flight private read before its first memory
    // exists. It still needs the connector tombstone for delayed capture.
    for root in [home.join("agents"), home.join("agents/.ephemeral")] {
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.to_string()),
        };
        for entry in entries {
            let entry = entry.map_err(|e| e.to_string())?;
            if !entry.file_type().map_err(|e| e.to_string())?.is_dir() { continue; }
            let Some(agent) = entry.file_name().to_str().map(str::to_owned) else { continue; };
            if duduclaw_core::is_valid_agent_id(&agent) { agents.push(agent); }
        }
    }
    agents.sort(); agents.dedup();
    Ok(agents)
}

async fn plan(engine: &SqliteMemoryEngine, agent: &str, connector: &str) -> Result<ForgetPlan, String> {
    match engine.plan_forget_connector(agent, connector, PlanOptions {
        ttl_minutes: Some(TTL_MINUTES), max_rows: None,
    }).await.map_err(|e| e.to_string())? {
        PlanOutcome::Planned(p) => Ok(*p),
        PlanOutcome::TooLarge { reason } => Err(reason),
        PlanOutcome::NothingToForget { .. } => Err("connector plan was not recorded".into()),
    }
}

fn digest(plans: &[ForgetPlan]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for p in plans {
        hasher.update(p.document.agent_id.as_bytes());
        hasher.update([0]);
        hasher.update(p.plan_hash.as_bytes());
        hasher.update([0]);
    }
    format!("{:x}", hasher.finalize())
}

#[derive(Clone)]
struct Batch {
    id: String,
    connector: String,
    name: String,
    expires: String,
    digest: String,
    status: String,
    approved_by: Option<String>,
    approval_id: Option<String>,
}

fn batch(home: &Path, id: &str) -> Result<Batch, String> {
    connection(home)?.query_row(
        "SELECT id,connector_ref,display_name,expires_at,digest,status,approved_by,approval_id FROM disconnect_forget_batches WHERE id=?1",
        [id], |r| Ok(Batch { id: r.get(0)?, connector: r.get(1)?, name: r.get(2)?, expires: r.get(3)?, digest: r.get(4)?, status: r.get(5)?, approved_by: r.get(6)?, approval_id: r.get(7)? }),
    ).map_err(|e| e.to_string())
}

async fn batch_plans(home: &Path, engine: &SqliteMemoryEngine, id: &str, applying: bool) -> Result<Vec<ForgetPlan>, String> {
    let ids = {
        let conn = connection(home)?;
        let mut stmt = conn.prepare(if applying {
            "SELECT COALESCE(apply_plan_id,approved_plan_id) FROM disconnect_forget_batch_plans WHERE batch_id=?1 ORDER BY agent_id"
        } else {
            "SELECT approved_plan_id FROM disconnect_forget_batch_plans WHERE batch_id=?1 ORDER BY agent_id"
        }).map_err(|e| e.to_string())?;
        let rows = stmt.query_map([id], |r| r.get(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<String>, _>>().map_err(|e| e.to_string())?
    };
    let mut out = Vec::new();
    for id in ids {
        out.push(engine.get_forget_plan(&id).await.map_err(|e| e.to_string())?.ok_or("missing batch plan")?);
    }
    Ok(out)
}

fn binding(b: &Batch) -> Binding<'_> {
    Binding { action: ACTION, target: &b.id, request_digest: b.digest.clone(),
        state: String::new(), state_policy: StatePolicy::MustMatch, push_scope: Some(&b.connector) }
}

async fn file_card(home: &Path, b: &Batch, plans: &[ForgetPlan]) -> Result<String, String> {
    let broker = ApprovalBroker::open(home)?;
    let counts: Vec<Value> = plans.iter().map(|p| json!({
        "agent_id": p.document.agent_id,
        "memories": p.document.body.targets.iter().filter(|t| t.store == "memories").count(),
        "key_facts": p.document.body.targets.iter().filter(|t| t.store == "key_facts").count(),
        "archive": p.document.body.archive_ids.len(),
        "unattributed": p.document.body.untracked_in_namespace,
        "system_kept": p.document.body.system_rows,
    })).collect();
    let summary = format!("斷開「{}」後，刪除可歸屬此連線的記憶。請逐一確認下方 {} 個員工或工具命名空間的範圍。",
        duduclaw_core::truncate_chars(&b.name, 120), plans.len());
    let ttl = chrono::DateTime::parse_from_rfc3339(&b.expires).map_err(|e| e.to_string())?
        .signed_duration_since(Utc::now()).num_seconds();
    if ttl <= 0 { return Err("disconnect approval expired".into()); }
    let total_memories: usize = plans.iter().map(|p| p.document.body.targets.len()).sum();
    let filing = Filing { agent_id: "system", summary: &summary, ttl_secs: ttl,
        extra: json!({"batch_id":b.id,"batch_digest":b.digest,"connector_ref":b.connector,
            "display_name":b.name,"counts_by_agent":counts,"counts":{"memories":total_memories},"origin":crate::memory_forget_approval::ORIGIN_DISCONNECT,
            "not_covered":NOT_COVERED}) };
    // Repair a crash after card creation but before saving its id without
    // recreating a card the Admin already denied in the meantime.
    let existing = broker.list_by_kind(crate::memory_forget_approval::ACTION_KIND_MEMORY_FORGET_SOURCE).await?
        .into_iter().filter(|record| gate::binding_of(record).is_some_and(|bound|
            bound.action == ACTION && bound.target == b.id && bound.request_digest == b.digest))
        .max_by(|a, b| a.created_at.cmp(&b.created_at));
    let id = match existing {
        Some(record) if matches!(record.status, crate::approval::ApprovalStatus::Approved | crate::approval::ApprovalStatus::Pending) => record.id,
        Some(_) => {
            connection(home)?.execute("UPDATE disconnect_forget_batches SET status='refused' WHERE id=?1 AND status='planned' AND digest=?2",
                params![b.id,b.digest]).map_err(|e| e.to_string())?;
            return Err("disconnect approval refused".into());
        }
        None => {
            let (result, _) = gate::gate(&broker, &crate::memory_forget_approval::SPEC,
                &binding(b), filing, None, Utc::now()).await?;
            match result {
                gate::Gate::Requested(id) | gate::Gate::Pending(id) | gate::Gate::AlreadyClaimed(id) => id,
                gate::Gate::Proceed(c) => c.id,
                gate::Gate::Throttled { .. } => return Err("forget approval queue is full".into()),
            }
        }
    }.to_string();
    connection(home)?.execute("UPDATE disconnect_forget_batches SET approval_id=?2,last_error=NULL WHERE id=?1 AND digest=?3",
        params![b.id, id, b.digest]).map_err(|e| e.to_string())?;
    Ok(id)
}

/// Called only after credentials/configuration have been removed successfully.
/// Planning failures never undo or conceal the completed disconnect.
pub async fn after_disconnect(home: &Path, connector: &str, display_name: &str, requested: bool) -> Value {
    if !requested { return json!({"status":"skipped"}); }
    if !crate::memory_forget_apply::enabled(home) {
        return json!({"status":"disabled","message":"記憶依來源刪除已關閉或設定無法讀取；連線已斷開，未建立刪除計畫。"});
    }
    match create_batch(home, connector, display_name).await {
        Ok((id, approval)) => json!({"status":"pending_approval","batch_id":id,"approval_id":approval,
            "approval_url":format!("/inbox?item={approval}")}),
        Err(error) => json!({"status":"failed","message":format!("連線已斷開，刪除計畫未完成：{error}")}),
    }
}

async fn create_batch(home: &Path, connector: &str, name: &str) -> Result<(String, String), String> {
    let engine = engine(home)?;
    let mut plans = Vec::new();
    for agent in namespaces(home)? { plans.push(plan(&engine, &agent, connector).await?); }
    let b = Batch { id: uuid::Uuid::new_v4().to_string(), connector: connector.into(), name: name.into(),
        expires: (Utc::now() + Duration::minutes(TTL_MINUTES)).to_rfc3339(), digest: digest(&plans),
        status: "planned".into(), approved_by: None, approval_id: None };
    {
        let mut conn = connection(home)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        tx.execute("INSERT INTO disconnect_forget_batches(id,connector_ref,display_name,created_at,expires_at,digest) VALUES(?1,?2,?3,?4,?5,?6)",
            params![b.id,b.connector,b.name,Utc::now().to_rfc3339(),b.expires,b.digest]).map_err(|e| e.to_string())?;
        for p in &plans {
            tx.execute("INSERT INTO disconnect_forget_batch_plans(batch_id,agent_id,approved_plan_id) VALUES(?1,?2,?3)",
                params![b.id,p.document.agent_id,p.plan_id]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    let approval = file_card(home, &b, &plans).await?;
    Ok((b.id, approval))
}

/// The decision is already durable when this is called. The sweep also
/// repairs a crash between the decision commit and this job transition.
pub async fn enqueue_approved(home: &Path, id: &str) -> Result<(), String> {
    let b = batch(home, id)?;
    if b.status != "planned" { return Ok(()); }
    let broker = ApprovalBroker::open(home)?;
    let verdict = gate::verdict(&broker, &crate::memory_forget_approval::SPEC,
        ACTION, &b.id, &b.digest, None, Utc::now()).await?;
    if let gate::Verdict::Approved { id: approval } = verdict {
        let rec = broker.get(&approval).await?.ok_or("approval missing")?;
        let actor = rec.decided_by.filter(|s| s.starts_with("dashboard:")).ok_or("dashboard approval required")?;
        connection(home)?.execute("UPDATE disconnect_forget_batches SET status='applying',approved_by=?2,lease_holder=NULL,lease_until=0 WHERE id=?1 AND status='planned' AND digest=?3",
            params![b.id,actor,b.digest]).map_err(|e| e.to_string())?;
    } else if matches!(verdict, gate::Verdict::Refused { .. } | gate::Verdict::Mismatch { .. }) {
        connection(home)?.execute("UPDATE disconnect_forget_batches SET status='refused' WHERE id=?1 AND status='planned' AND digest=?2",
            params![b.id,b.digest]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn renew(home: &Path, id: &str, holder: &str) -> Result<(), String> {
    let n = connection(home)?.execute("UPDATE disconnect_forget_batches SET lease_until=?3 WHERE id=?1 AND status='applying' AND lease_holder=?2 AND lease_until>?4",
        params![id,holder,Utc::now().timestamp()+LEASE_SECONDS,Utc::now().timestamp()]).map_err(|e| e.to_string())?;
    if n != 1 { return Err("disconnect job lease lost".into()); }
    Ok(())
}

async fn execute(home: &Path, engine: &SqliteMemoryEngine, b: &Batch, holder: &str) -> Result<(), String> {
    if !crate::memory_forget_apply::enabled(home) { return Err("forget_source disabled or unreadable".into()); }
    let approved = batch_plans(home, engine, &b.id, false).await?;
    let current = batch_plans(home, engine, &b.id, true).await?;
    let mut replanned = Vec::new();
    let mut expanded = false;
    // Recompute every namespace before the first deletion. A new namespace
    // with matching rows is scope growth too, even if it was absent at filing.
    let mut agents = namespaces(home)?;
    agents.extend(approved.iter().map(|p| p.document.agent_id.clone()));
    agents.sort(); agents.dedup();
    for agent in agents {
        renew(home, &b.id, holder)?;
        if let Some(p) = current.iter().find(|p| p.document.agent_id == agent && p.status == "applied") {
            replanned.push(p.clone());
            continue;
        }
        let next = plan(engine, &agent, &b.connector).await?;
        let old = approved.iter().find(|p| p.document.agent_id == agent);
        if old.is_some_and(|p| !duduclaw_memory::connector_plan_is_subset(p, &next))
            || (old.is_none() && (!next.document.body.targets.is_empty() || !next.document.body.archive_ids.is_empty() || !next.document.body.lineage_only.is_empty())) {
            expanded = true;
        }
        replanned.push(next);
    }
    renew(home, &b.id, holder)?;
    {
        let mut conn = connection(home)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let owned: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM disconnect_forget_batches WHERE id=?1 AND lease_holder=?2 AND lease_until>?3 AND status='applying')",
            params![b.id,holder,Utc::now().timestamp()], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !owned { return Err("disconnect job lease lost".into()); }
        for p in &replanned {
            tx.execute(if expanded {
                "INSERT INTO disconnect_forget_batch_plans(batch_id,agent_id,approved_plan_id,apply_plan_id) VALUES(?1,?2,?3,?3) ON CONFLICT(batch_id,agent_id) DO UPDATE SET approved_plan_id=excluded.approved_plan_id,apply_plan_id=excluded.apply_plan_id"
            } else {
                "INSERT INTO disconnect_forget_batch_plans(batch_id,agent_id,approved_plan_id,apply_plan_id) VALUES(?1,?2,?3,?3) ON CONFLICT(batch_id,agent_id) DO UPDATE SET apply_plan_id=excluded.apply_plan_id"
            }, params![b.id,p.document.agent_id,p.plan_id]).map_err(|e| e.to_string())?;
        }
        if expanded {
            tx.execute("UPDATE disconnect_forget_batches SET status='planned',digest=?2,approval_id=NULL,approved_by=NULL,lease_holder=NULL,lease_until=0,expires_at=?3,last_error='scope expanded; approval required' WHERE id=?1",
                params![b.id,digest(&replanned),(Utc::now()+Duration::minutes(TTL_MINUTES)).to_rfc3339()]).map_err(|e| e.to_string())?;
        }
        tx.commit().map_err(|e| e.to_string())?;
    }
    if expanded {
        file_card(home, &batch(home, &b.id)?, &replanned).await?;
        return Ok(());
    }
    let mut counts = Vec::new();
    for p in &replanned {
        renew(home, &b.id, holder)?;
        let (outcome, steps) = crate::memory_forget_apply::apply(home, engine, p).await.map_err(|e| e.to_string())?;
        if !matches!(outcome, ApplyOutcome::Applied(_) | ApplyOutcome::AlreadyApplied) {
            return Err(format!("forget apply refused: {outcome:?}"));
        }
        if !steps.is_some_and(|s| s.complete()) { return Err("forget follow-up steps incomplete".into()); }
        let report = engine.get_forget_apply_report(&p.plan_id).await.map_err(|e| e.to_string())?
            .ok_or("durable apply report missing")?;
        counts.push(json!({"agent_id":report.agent_id,"memories_deleted":report.memories_deleted,
            "key_facts_deleted":report.key_facts_deleted,"archive_deleted":report.archive_deleted,
            "unattributed":report.untracked_in_namespace_at_apply,"system_kept":report.system_rows}));
    }
    renew(home, &b.id, holder)?;
    let completed = Utc::now().to_rfc3339();
    let receipt = json!({"id":b.id,"batch_id":b.id,"connector_ref":b.connector,"display_name":b.name,
        "completed_at":completed,"approved_by":b.approved_by,"counts":counts,"not_covered":NOT_COVERED});
    {
        let mut conn = connection(home)?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(|e| e.to_string())?;
        let n = tx.execute("UPDATE disconnect_forget_batches SET status='complete',lease_holder=NULL,lease_until=0,last_error=NULL WHERE id=?1 AND status='applying' AND lease_holder=?2 AND lease_until>?3",
            params![b.id,holder,Utc::now().timestamp()]).map_err(|e| e.to_string())?;
        if n != 1 { return Err("disconnect job lease lost".into()); }
        tx.execute("INSERT INTO forget_receipts(id,batch_id,completed_at,receipt_json) VALUES(?1,?1,?2,?3)",
            params![b.id,completed,receipt.to_string()]).map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?;
    }
    duduclaw_security::audit::append_audit_event(home, &duduclaw_security::audit::AuditEvent::new(
        "memory_disconnect_forgotten", "system", duduclaw_security::audit::Severity::Warning, receipt));
    Ok(())
}

pub async fn sweep(home: &Path) -> Result<(), String> {
    if !home.join("memory.db").is_file() { return Ok(()); }
    use fs2::FileExt;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
        .open(home.join("disconnect_forget_jobs.lock")).map_err(|e| e.to_string())?;
    match lock.try_lock_exclusive() {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    // Keep the OS lock across awaits: a slow live worker cannot lose its
    // plan pointer to a second process after the durable lease expires.
    let _lock = lock;
    let engine = engine(home)?;
    let ids = {
        let conn = connection(home)?;
        let mut stmt = conn.prepare("SELECT id FROM disconnect_forget_batches WHERE status IN ('planned','applying') ORDER BY created_at LIMIT 100")
            .map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| r.get(0)).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<String>, _>>().map_err(|e| e.to_string())?
    };
    for id in ids {
        if let Err(error) = sweep_one(home, &engine, &id).await {
            connection(home)?.execute("UPDATE disconnect_forget_batches SET last_error=?2 WHERE id=?1 AND status!='complete'",
                params![id,duduclaw_core::truncate_chars(&error,512)]).map_err(|e| e.to_string())?;
            tracing::warn!(batch = %id, %error, "disconnect forget deferred");
        }
    }
    Ok(())
}

async fn sweep_one(home: &Path, engine: &SqliteMemoryEngine, id: &str) -> Result<(), String> {
    let mut b = batch(home, id)?;
    if b.status == "planned" {
        if chrono::DateTime::parse_from_rfc3339(&b.expires).map_err(|e| e.to_string())? <= Utc::now() {
            connection(home)?.execute("UPDATE disconnect_forget_batches SET status='expired' WHERE id=?1 AND status='planned'", [id])
                .map_err(|e| e.to_string())?;
            return Ok(());
        }
        if b.approval_id.is_none() {
            let plans = batch_plans(home, engine, id, false).await?;
            file_card(home, &b, &plans).await?;
        }
        enqueue_approved(home, id).await?;
        b = batch(home, id)?;
    }
    if b.status != "applying" { return Ok(()); }
    let holder = uuid::Uuid::new_v4().to_string();
    let claimed = connection(home)?.execute("UPDATE disconnect_forget_batches SET lease_holder=?2,lease_until=?3 WHERE id=?1 AND status='applying' AND lease_until<=?4",
        params![id,holder,Utc::now().timestamp()+LEASE_SECONDS,Utc::now().timestamp()]).map_err(|e| e.to_string())?;
    if claimed == 0 { return Ok(()); }
    let result = execute(home, engine, &b, &holder).await;
    connection(home)?.execute("UPDATE disconnect_forget_batches SET lease_holder=NULL,lease_until=0 WHERE id=?1 AND lease_holder=?2",
        params![id,holder]).map_err(|e| e.to_string())?;
    result
}

pub fn receipts(home: &Path, id: Option<&str>) -> Result<Value, String> {
    if !home.join("memory.db").is_file() {
        return if id.is_none() { Ok(json!({"receipts":[]})) } else { Err("receipt not found".into()) };
    }
    let conn = connection(home)?;
    if let Some(id) = id {
        let raw: Option<String> = conn.query_row("SELECT receipt_json FROM forget_receipts WHERE id=?1", [id], |r| r.get(0))
            .optional().map_err(|e| e.to_string())?;
        let value: Value = serde_json::from_str(&raw.ok_or("receipt not found")?).map_err(|e| e.to_string())?;
        return Ok(json!({"receipt":value}));
    }
    let mut stmt = conn.prepare("SELECT receipt_json FROM forget_receipts ORDER BY completed_at DESC LIMIT 200").map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| r.get::<_,String>(0)).map_err(|e| e.to_string())?;
    let mut values = Vec::new();
    for row in rows { values.push(serde_json::from_str::<Value>(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())?); }
    Ok(json!({"receipts":values}))
}

#[cfg(test)]
#[path = "disconnect_forget_tests.rs"]
mod tests;
