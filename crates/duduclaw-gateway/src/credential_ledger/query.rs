use super::*;

fn metadata(value: &str, limit: usize) -> Result<String, String> {
    if value.chars().count() > limit || value.chars().any(char::is_control) {
        return Err("invalid credential metadata".into());
    }
    Ok(credential_detect::mask(value))
}

fn row_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    Ok(json!({
        "id":row.get::<_,String>(0)?, "kind":row.get::<_,String>(1)?,
        "account_hint":row.get::<_,Option<String>>(2)?, "service":row.get::<_,Option<String>>(3)?,
        "service_source":row.get::<_,String>(4)?, "source_kind":row.get::<_,String>(5)?,
        "source_ref":row.get::<_,String>(6)?, "provided_by":row.get::<_,String>(7)?,
        "status":row.get::<_,String>(8)?, "first_seen_at":row.get::<_,String>(9)?,
        "last_seen_at":row.get::<_,String>(10)?, "last_used_at":row.get::<_,Option<String>>(11)?,
        "expires_at":row.get::<_,Option<String>>(12)?, "rotation_due_at":row.get::<_,Option<String>>(13)?,
        "note":row.get::<_,Option<String>>(14)?,
    }))
}
const COLUMNS: &str = "id,kind,account_hint,service,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at,last_used_at,expires_at,rotation_due_at,note";

fn add_holders(db: &Connection, row: &mut Value) -> Result<(), String> {
    let mut stmt = db.prepare("SELECT holder_kind,holder_id,granted_at,revoked_at FROM credential_holders WHERE credential_id=?1 ORDER BY holder_kind,holder_id").map_err(|_| UNAVAILABLE)?;
    let holders = stmt.query_map([row["id"].as_str().ok_or(UNAVAILABLE)?], |r| Ok(json!({
        "holder_kind":r.get::<_,String>(0)?, "holder_id":r.get::<_,String>(1)?,
        "granted_at":r.get::<_,String>(2)?, "revoked_at":r.get::<_,Option<String>>(3)?,
    }))).map_err(|_| UNAVAILABLE)?.collect::<Result<Vec<_>,_>>().map_err(|_| UNAVAILABLE)?;
    row["holders"] = json!(holders);
    Ok(())
}

pub fn list(home: &Path, params: &Value) -> Result<Value, String> {
    let db = open(home)?;
    let filter = |key| -> Result<Option<&str>, String> {
        match params.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) if s.len() <= 200 => Ok(Some(s)),
            _ => Err("invalid credential filter".into()),
        }
    };
    let (kind, status, service) = (filter("kind")?, filter("status")?, filter("service")?);
    let limit = params.get("limit").and_then(Value::as_u64).unwrap_or(100).clamp(1, 500) as i64;
    let offset = params.get("offset").and_then(Value::as_u64).unwrap_or(0).min(ROW_LIMIT as u64) as i64;
    let predicate = "(?1 IS NULL OR kind=?1) AND (?2 IS NULL OR status=?2) AND (?3 IS NULL OR service=?3)";
    let mut stmt = db.prepare(&format!("SELECT {COLUMNS} FROM credentials WHERE {predicate} ORDER BY first_seen_at DESC,id LIMIT ?4 OFFSET ?5")).map_err(|_| UNAVAILABLE)?;
    let mut rows = stmt.query_map(params![kind,status,service,limit,offset], row_value).map_err(|_| UNAVAILABLE)?
        .collect::<Result<Vec<_>,_>>().map_err(|_| UNAVAILABLE)?;
    for row in &mut rows { add_holders(&db, row)?; }
    let total: i64 = db.query_row(&format!("SELECT count(*) FROM credentials WHERE {predicate}"), params![kind,status,service], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
    if params.get("export").and_then(Value::as_bool) == Some(true) {
        duduclaw_security::audit::try_append_audit_event(home, &AuditEvent::new(
            "credential_exported", "credential-ledger", Severity::Info,
            json!({"count":rows.len(),"offset":offset}),
        )).map_err(|_| "credential export audit unavailable")?;
    }
    Ok(json!({"credentials":rows,"mode":mode(home),"total":total}))
}

pub fn get(home: &Path, id: &str) -> Result<Value, String> {
    let db = open(home)?;
    let mut row = db.query_row(&format!("SELECT {COLUMNS} FROM credentials WHERE id=?1"), [id], row_value)
        .optional().map_err(|_| UNAVAILABLE)?.ok_or("credential not found")?;
    add_holders(&db, &mut row)?;
    Ok(row)
}

pub fn update(home: &Path, id: &str, params: &Value) -> Result<(), String> {
    mutate(home, id, params, None)
}

pub fn declare(home: &Path, agent: &str, credential_id: &str, service: &str, note: Option<&str>) -> Result<(), String> {
    let mut params = json!({"service":service});
    if let Some(note) = note { params["note"] = json!(note); }
    mutate(home, credential_id, &params, Some(agent))
}

fn mutate(home: &Path, id: &str, input: &Value, employee: Option<&str>) -> Result<(), String> {
    let Some(object) = input.as_object() else { return Err("invalid credential update".into()) };
    if object.keys().any(|k| !matches!(k.as_str(), "id" | "credential_id" | "service" | "note" | "status")) {
        return Err("credential values are not accepted".into());
    }
    let string_field = |key, limit| -> Result<Option<String>, String> {
        match input.get(key) {
            None => Ok(None),
            Some(Value::String(value)) => metadata(value, limit).map(Some),
            _ => Err("invalid credential metadata".into()),
        }
    };
    let service = string_field("service", 160)?;
    let note = string_field("note", 1000)?;
    let status = string_field("status", 32)?;
    if let Some(status) = status.as_deref() {
        if employee.is_some() || !matches!(status, "active" | "revoked" | "rotated" | "expired" | "false_positive") {
            return Err("invalid credential status".into());
        }
    }
    let mut db = open(home)?;
    let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(|_| UNAVAILABLE)?;
    let source: String = tx.query_row("SELECT service_source FROM credentials WHERE id=?1", [id], |r| r.get(0))
        .optional().map_err(|_| UNAVAILABLE)?.ok_or("credential not found")?;
    if let Some(agent) = employee {
        let holds: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM credential_holders WHERE credential_id=?1 AND holder_kind='agent' AND holder_id=?2 AND revoked_at IS NULL)", params![id,agent], |r| r.get(0)).map_err(|_| UNAVAILABLE)?;
        if !holds { return Err("credential is not held by this employee".into()); }
        if source == "admin" { return Err("administrator credential metadata cannot be overridden".into()); }
    }
    if let Some(service) = &service {
        tx.execute("UPDATE credentials SET service=?2,service_source=?3 WHERE id=?1", params![id,service,if employee.is_some() {"employee"} else {"admin"}]).map_err(|_| UNAVAILABLE)?;
    }
    if let Some(note) = note {
        tx.execute("UPDATE credentials SET note=?2 WHERE id=?1", params![id,note]).map_err(|_| UNAVAILABLE)?;
    }
    if let Some(status) = &status {
        tx.execute("UPDATE credentials SET status=?2 WHERE id=?1", params![id,status]).map_err(|_| UNAVAILABLE)?;
    }
    tx.commit().map_err(|_| UNAVAILABLE)?;
    if service.is_some() { event(home,"credential_service_set",json!({"credential_id":id,"authority":if employee.is_some() {"employee"} else {"admin"}})); }
    if let Some(status) = status { event(home,"credential_status_changed",json!({"credential_id":id,"status":status})); }
    Ok(())
}
