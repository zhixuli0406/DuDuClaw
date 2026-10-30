//! File-then-retry approvals for native CLI tools. Only the gateway calls this.

use super::*;
use super::action_grant::{ActionGrant, ACTION_GRANT_KEY, verify_and_consume};
use serde_json::json;

const KIND: &str = "cli_egress";
const MAX_OPEN: usize = 20;
const TTL_SECONDS: i64 = 1800;

pub(crate) enum Gate {
    Requested(ApprovalId),
    Proceed,
}

pub(crate) async fn check(
    broker: &ApprovalBroker, agent: &str, turn: &str, tool: &str,
    args: &Value, sources: &[String],
) -> Result<Gate, String> {
    // The singleton gateway serializes lookup + insertion across its hook
    // requests. Consumption itself remains a database CAS.
    static SERIAL: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    let _serial = SERIAL.get_or_init(|| Mutex::new(())).lock().await;
    let payload = json!({"name": tool, "arguments": args});
    let grant = ActionGrant::build(agent, tool, &payload);
    let principal = raising_principal();
    let ids: Vec<String> = {
        let conn = broker.store.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT id FROM approvals WHERE agent_id=?1 AND action_kind=?2
             AND status IN ('pending','approved')
             AND julianday(created_at) + ttl_seconds / 86400.0 > julianday('now')
             ORDER BY created_at DESC LIMIT 21"
        ).map_err(|e| e.to_string())?;
        let rows = stmt.query_map(params![agent, KIND], |row| row.get(0))
            .map_err(|e| e.to_string())?.collect::<Result<_, _>>().map_err(|e| e.to_string())?;
        rows
    };
    let mut open_count = ids.len();
    for id in &ids {
        let id = ApprovalId::from(id.clone());
        let Some(rec) = broker.get(&id).await? else { return Err("審批紀錄已變動，請重試。".into()); };
        if rec.payload.get("turn_id").and_then(Value::as_str) != Some(turn)
            || !rec.payload.get(ACTION_GRANT_KEY).is_some_and(|v| grant.matches_stored(v)) {
            continue;
        }
        let covered = rec.on_behalf_of == principal && rec.payload["egress_guard"]["sources"].as_array()
            .is_some_and(|old| sources.iter().all(|s| old.iter().any(|v| v.as_str() == Some(s))));
        if !covered {
            broker.invalidate_request(&id, "egress_sources_changed").await?;
            open_count -= 1;
            continue;
        }
        if rec.status == ApprovalStatus::Pending { return Ok(Gate::Requested(id)); }
        verify_and_consume(broker, &id, &grant).await
            .map_err(|_| "這次核准已使用、過期或參數不符；請重新提出同一動作。".to_string())?;
        return Ok(Gate::Proceed);
    }
    if open_count >= MAX_OPEN {
        return Err("外送審批已達待處理上限，請管理員先處理儀表板中的申請再重試。".into());
    }
    let mut payload = payload;
    payload[ACTION_GRANT_KEY] = grant.to_json();
    payload["turn_id"] = json!(turn);
    payload["egress_guard"] = json!({"reason": "egress_guard", "sources": sources});
    // Full arguments are kept in the Admin-only expanded card. No caller
    // supplies a digest, summary or display string independently of them.
    let rec = ApprovalRecord {
        id: ApprovalId::new(), agent_id: agent.into(), action_kind: KIND.into(),
        summary: format!("CLI 工具「{tool}」對外傳送資料；請展開並確認完整參數。"),
        payload, status: ApprovalStatus::Pending, created_at: Utc::now().to_rfc3339(),
        decided_at: None, decided_by: None, ttl_seconds: TTL_SECONDS,
        notify_channel: None, notify_chat_id: None, reminded_at: None, simulation: None,
        request_kind: RequestKind::Approval, binding: None, answer: None,
        invalidated_reason: None, on_behalf_of: principal,
    };
    // A hook must return promptly; this ticket is visible in the dashboard
    // without waiting for a channel provider or any model call.
    broker.store.insert(&rec).await?;
    Ok(Gate::Requested(rec.id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn egress_hook_binding_reuse_and_single_consumption() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let args = json!({"url": "https://example.org/private?value=甲", "prompt": "read"});
        let sources = vec!["memory_search".into()];
        let Gate::Requested(id) = check(&broker, "bot", "turn", "WebFetch", &args, &sources).await.unwrap() else { panic!() };
        let reordered: Value = serde_json::from_str(r#"{"prompt":"read","url":"https://example.org/private?value=甲"}"#).unwrap();
        let Gate::Requested(same) = check(&broker, "bot", "turn", "WebFetch", &reordered, &sources).await.unwrap() else { panic!() };
        assert_eq!(id, same);
        assert!(broker.decide(&id, true, "channel:admin").await.is_err());
        broker.store.conn.lock().await.execute("UPDATE approvals SET status='approved' WHERE id=?1", [id.as_str()]).unwrap();
        let changed = json!({"url": "https://example.org/other", "prompt": "read"});
        assert!(matches!(check(&broker, "bot", "turn", "WebFetch", &changed, &sources).await.unwrap(), Gate::Requested(_)));
        assert!(matches!(check(&broker, "bot", "different-turn", "WebFetch", &args, &sources).await.unwrap(), Gate::Requested(_)));
        assert!(matches!(check(&broker, "bot", "turn", "WebFetch", &args, &sources).await.unwrap(), Gate::Proceed));
        assert!(matches!(check(&broker, "bot", "turn", "WebFetch", &args, &sources).await.unwrap(), Gate::Requested(_)));
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Invalidated);
    }

    #[tokio::test]
    async fn egress_hook_changed_command_or_sources_never_borrows_approval() {
        let broker = ApprovalBroker::new(std::sync::Arc::new(ApprovalStore::open_in_memory().unwrap()));
        let args = json!({"command": "curl https://example.org/a"});
        let sources = vec!["memory_search".into()];
        let Gate::Requested(id) = check(&broker, "bot", "turn", "Bash", &args, &sources).await.unwrap() else { panic!() };
        broker.store.conn.lock().await.execute("UPDATE approvals SET status='approved' WHERE id=?1", [id.as_str()]).unwrap();
        for (args, sources) in [(json!({"command": "curl https://example.org/b"}), sources.clone()),
            (args.clone(), vec!["memory_search".into(), "mail_read".into()])] {
            assert!(matches!(check(&broker, "bot", "turn", "Bash", &args, &sources).await.unwrap(), Gate::Requested(_)));
        }
        assert_eq!(broker.get(&id).await.unwrap().unwrap().status, ApprovalStatus::Invalidated);
        let sources = vec!["memory_search".into(), "mail_read".into()];
        let Gate::Requested(replacement) = check(&broker, "bot", "turn", "Bash", &args, &sources).await.unwrap() else { panic!() };
        broker.store.conn.lock().await.execute("UPDATE approvals SET status='approved' WHERE id=?1", [replacement.as_str()]).unwrap();
        let (a, b) = tokio::join!(check(&broker, "bot", "turn", "Bash", &args, &sources), check(&broker, "bot", "turn", "Bash", &args, &sources));
        assert_ne!(matches!(a.unwrap(), Gate::Proceed), matches!(b.unwrap(), Gate::Proceed));
    }
}
