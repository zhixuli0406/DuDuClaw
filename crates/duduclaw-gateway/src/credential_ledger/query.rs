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
        "note":row.get::<_,Option<String>>(14)?, "note_source":row.get::<_,String>(15)?,
        "source_ordinal":row.get::<_,i64>(16)?,
    }))
}
const COLUMNS: &str = "id,kind,account_hint,service,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at,last_used_at,expires_at,rotation_due_at,note,note_source,source_ordinal";

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

pub fn declare(home: &Path, agent: &str, credential_id: &str, service: &str) -> Result<(), String> {
    mutate(home, credential_id, &json!({"service":service}), Some(agent))
}

fn employee_service(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    let starts_with_identifier = chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    if !starts_with_identifier || value.len() > 64
        || !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
        || !matches!(credential_detect::detect(value), Ok(findings) if findings.is_empty())
    {
        return Err("service must be a non-secret identifier of 1 to 64 characters".into());
    }
    Ok(value.to_owned())
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
    if employee.is_some() && object.keys().any(|key| key != "service") {
        return Err("employees may declare only a service identifier".into());
    }
    let service = if employee.is_some() {
        Some(employee_service(input.get("service").and_then(Value::as_str).ok_or("invalid credential service")?)?)
    } else {
        string_field("service", 160)?
    };
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
        tx.execute("UPDATE credentials SET note=?2,note_source='admin' WHERE id=?1", params![id,note]).map_err(|_| UNAVAILABLE)?;
    }
    if let Some(status) = &status {
        tx.execute("UPDATE credentials SET status=?2 WHERE id=?1", params![id,status]).map_err(|_| UNAVAILABLE)?;
    }
    tx.commit().map_err(|_| UNAVAILABLE)?;
    if service.is_some() { event(home,"credential_service_set",json!({"credential_id":id,"authority":if employee.is_some() {"employee"} else {"admin"}})); }
    if let Some(status) = status { event(home,"credential_status_changed",json!({"credential_id":id,"status":status})); }
    Ok(())
}

#[cfg(test)]
mod metadata_security_tests {
    use super::*;

    // Security regression tests added for F5; not run (owner forbids builds).
    #[test]
    fn employee_service_rejects_free_text_and_detected_credentials() {
        for value in ["", "CRM", "two words", "password: fixture", "mail/box", "ümlaut", "-mail", "mail\n"] {
            assert!(employee_service(value).is_err(), "accepted {value:?}");
        }
        assert!(employee_service(&"a".repeat(65)).is_err());
        let key = format!("sk-{}", "a".repeat(40));
        assert!(employee_service(&key).is_err());
        for value in ["mail", "a-b.c_d", "0", &"a".repeat(64)] {
            assert_eq!(employee_service(value).unwrap(), value);
        }
    }

    #[test]
    fn admin_note_and_service_authority_are_independent_and_final() {
        let home = tempfile::tempdir().unwrap();
        let db = open(home.path()).unwrap();
        db.execute("INSERT INTO credentials(id,kind,service_source,source_kind,source_ref,provided_by,status,first_seen_at,last_seen_at) VALUES('cred_test','password','detected','chat','source','sender','active','0','0')", []).unwrap();
        db.execute("INSERT INTO credential_holders(credential_id,holder_kind,holder_id,granted_at,granted_by) VALUES('cred_test','agent','bot','0','routing')", []).unwrap();
        update(home.path(), "cred_test", &json!({"note":"administrator note"})).unwrap();
        declare(home.path(), "bot", "cred_test", "mail").unwrap();
        assert!(mutate(home.path(), "cred_test", &json!({"note":"employee replacement"}), Some("bot")).is_err());
        let row = get(home.path(), "cred_test").unwrap();
        assert_eq!(row["note"], "administrator note");
        assert_eq!(row["note_source"], "admin");
        assert_eq!(row["service_source"], "employee");
        update(home.path(), "cred_test", &json!({"service":"Approved service"})).unwrap();
        assert!(declare(home.path(), "bot", "cred_test", "changed").is_err());
        update(home.path(), "cred_test", &json!({"note":"updated administrator note"})).unwrap();
        let row = get(home.path(), "cred_test").unwrap();
        assert_eq!(row["service"], "Approved service");
        assert_eq!(row["service_source"], "admin");
        assert_eq!(row["note_source"], "admin");
        db.execute("UPDATE credentials SET source_ordinal=1 WHERE id='cred_test'", []).unwrap();
        assert_eq!(get(home.path(), "cred_test").unwrap()["source_ordinal"], 1);
        assert_eq!(list(home.path(), &json!({})).unwrap()["credentials"][0]["source_ordinal"], 1);
    }
}
