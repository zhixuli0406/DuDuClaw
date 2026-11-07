//! RM-039 — "my memory": a staff member lists and deletes what the AI
//! employees automatically remember **about them**.
//!
//! Authorization is the subject set, not agent bindings: the caller's
//! subjects are resolved server-side from the authenticated identity
//! (`memory_privacy::self_subjects` — `user:<dashboard id>`, their WebChat
//! identity and every *verified* linked channel identity) and every read and
//! delete is filtered by them in SQL. A fact about the caller is theirs to see
//! wherever it was learned, so rows are listed across every AI employee's
//! namespace. Nothing the client sends can widen the set; an id that is not
//! one of the caller's rows is answered exactly like an id that does not
//! exist.
//!
//! Rows without a person subject (plain conversation facts, explicit
//! `memory_store` notes) are not "about" anyone and are not listed here.

use super::*;

/// Page size cap for `me.memory.list`.
pub(crate) const MY_MEMORY_PAGE_MAX: usize = 50;
const MY_MEMORY_PAGE_DEFAULT: usize = 20;

/// Explicit authorization registry: any logged-in dashboard user.
pub(crate) fn minimum_role(method: &str) -> Option<UserRole> {
    match method {
        "me.memory.list" | "me.memory.forget" => Some(UserRole::Employee),
        _ => None,
    }
}

const NOT_FOUND: &str = "找不到這筆記憶，可能已被刪除。";

/// `source_kind` shown to the user for a row.
fn source_kind(f: &duduclaw_memory::SubjectFact) -> &'static str {
    match f.source_event.as_str() {
        "conversation_distill" => "conversation",
        "user_profile" => "profile",
        "mcp_internal" | "mcp_external" => "explicit",
        "footprint_distill" => "footprint",
        "reflexion_consolidation" => "reflexion",
        _ => "other",
    }
}

fn is_explicit(f: &duduclaw_memory::SubjectFact) -> bool {
    f.metadata.get("explicit").and_then(Value::as_bool) == Some(true)
        || matches!(f.source_event.as_str(), "mcp_internal" | "mcp_external")
}

/// Human rendering: profile traits are stored as `predicate: value`; show the
/// zh-TW label of the predicate instead of the internal token.
fn render_text(f: &duduclaw_memory::SubjectFact) -> String {
    if f.source_event == "user_profile" {
        if let (Some(p), Some(o)) = (f.predicate.as_deref(), f.object.as_deref()) {
            return format!("{}：{}", crate::profile_distill::predicate_label_zh(p), o);
        }
    }
    f.content.clone()
}

/// Opaque keyset cursor: `<timestamp>|<id>` (neither contains `|`).
fn encode_cursor(f: &duduclaw_memory::SubjectFact) -> String {
    format!("{}|{}", f.timestamp, f.id)
}

fn decode_cursor(raw: &str) -> Option<(String, String)> {
    let (ts, id) = raw.split_once('|')?;
    let ok = |s: &str| !s.is_empty() && s.len() <= 128 && !s.chars().any(char::is_control);
    (ok(ts) && ok(id)).then(|| (ts.to_string(), id.to_string()))
}

impl MethodHandler {
    /// The caller's subjects. `Err` = the identity could not be resolved
    /// (refuse rather than fall back to a partial set).
    async fn my_memory_subjects(&self, ctx: &UserContext) -> Result<Vec<String>, String> {
        let uid = ctx.user_id.trim().to_string();
        // The shared bootstrap / legacy-token principal is not a person.
        if uid.is_empty() || crate::profile_distill::is_anonymous(&uid) {
            return Err("請使用個人 Dashboard 帳號登入後再查看「我的記憶」。".into());
        }
        let channels: Vec<(String, String)> = match self.user_db.read().await.clone() {
            Some(db) => {
                let id = uid.clone();
                match tokio::task::spawn_blocking(move || db.verified_channels_for_user(&id)).await
                {
                    Ok(Ok(rows)) => rows
                        .into_iter()
                        .map(|c| (c.channel, c.channel_user_id))
                        .collect(),
                    _ => return Err("無法讀取帳號資料。".into()),
                }
            }
            None => Vec::new(),
        };
        Ok(crate::memory_privacy::self_subjects(&uid, &channels))
    }

    pub(crate) async fn handle_my_memory_rpc(
        &self,
        method: &str,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let subjects = match self.my_memory_subjects(ctx).await {
            Ok(s) => s,
            Err(e) => return WsFrame::error_response("", &e),
        };
        match method {
            "me.memory.list" => self.my_memory_list(&subjects, &params).await,
            "me.memory.forget" => self.my_memory_forget(&subjects, &params, ctx).await,
            _ => WsFrame::error_response("", "不支援此操作。"),
        }
    }

    async fn my_memory_list(&self, subjects: &[String], params: &Value) -> WsFrame {
        let limit = params
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(MY_MEMORY_PAGE_DEFAULT)
            .clamp(1, MY_MEMORY_PAGE_MAX);
        let cursor = match params.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.is_empty() => None,
            Some(Value::String(s)) => match decode_cursor(s) {
                Some(c) => Some(c),
                None => return WsFrame::error_response("", "cursor 無效"),
            },
            Some(_) => return WsFrame::error_response("", "cursor 無效"),
        };
        let empty = json!({ "items": [], "next_cursor": Value::Null });
        let db_path = self.home_dir.join("memory.db");
        if !db_path.exists() {
            return WsFrame::ok_response("", empty);
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"))
            }
        };
        let before = cursor.as_ref().map(|(t, i)| (t.as_str(), i.as_str()));
        let mut rows = match engine
            .list_current_by_subjects(subjects, before, limit + 1)
            .await
        {
            Ok(r) => r,
            Err(e) => return WsFrame::error_response("", &format!("讀取記憶失敗：{e}")),
        };
        let has_more = rows.len() > limit;
        rows.truncate(limit);
        let next_cursor = if has_more {
            rows.last().map(encode_cursor)
        } else {
            None
        };
        let items: Vec<Value> = rows
            .iter()
            .map(|f| {
                json!({
                    "id": f.id,
                    "agent": f.agent_id,
                    "text": render_text(f),
                    "created_at": f.timestamp,
                    "source_kind": source_kind(f),
                    "explicit": is_explicit(f),
                })
            })
            .collect();
        WsFrame::ok_response("", json!({ "items": items, "next_cursor": next_cursor }))
    }

    async fn my_memory_forget(
        &self,
        subjects: &[String],
        params: &Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let Some(id) = params
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 128)
        else {
            return WsFrame::error_response("", "id is required");
        };
        let db_path = self.home_dir.join("memory.db");
        if !db_path.exists() {
            return WsFrame::error_response("", NOT_FOUND);
        }
        let engine = match SqliteMemoryEngine::new(&db_path) {
            Ok(e) => e,
            Err(e) => {
                return WsFrame::error_response("", &format!("Failed to open memory db: {e}"))
            }
        };
        let row = match engine.subject_fact_by_id(id).await {
            Ok(Some(r)) if subjects.iter().any(|s| *s == r.subject) => r,
            Ok(_) => return WsFrame::error_response("", NOT_FOUND),
            Err(e) => return WsFrame::error_response("", &format!("讀取記憶失敗：{e}")),
        };

        // Re-learning guard first: if the delete below succeeds but this
        // failed, the next conversation could quietly put the fact back.
        if let Some(predicate) = row.predicate.as_deref() {
            let third = row.object.as_deref().unwrap_or(&row.content);
            let key = crate::memory_privacy::forgotten::key(&row.subject, predicate, third);
            if let Err(e) = crate::memory_privacy::forgotten::record(&self.home_dir, &key) {
                warn!("me.memory.forget: could not record forgotten key ({})", e.kind());
                return WsFrame::error_response("", "無法記錄刪除，請稍後再試。");
            }
        }

        let forgotten = match engine.forget(&row.agent_id, &row.id).await {
            Ok(f) => f,
            Err(e) => return WsFrame::error_response("", &format!("刪除記憶失敗：{e}")),
        };
        if !forgotten {
            return WsFrame::error_response("", NOT_FOUND);
        }

        // Audit: who removed which row, never the content.
        duduclaw_security::audit::append_audit_event(
            &self.home_dir,
            &duduclaw_security::audit::AuditEvent::new(
                "memory_self_forgotten",
                row.agent_id.clone(),
                duduclaw_security::audit::Severity::Info,
                json!({
                    "memory_id": row.id,
                    "user_id": ctx.user_id,
                    "source_kind": source_kind(&row),
                    "explicit": is_explicit(&row),
                }),
            ),
        );
        crate::dashboard_feedback::emit(
            &self.home_dir,
            crate::dashboard_feedback::EV_MEMORY_CHANGED,
            json!({ "action": "self_forgotten", "agent_id": row.agent_id }),
        )
        .await;

        WsFrame::ok_response(
            "",
            json!({ "forgotten": true, "id": row.id, "agent": row.agent_id }),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_memory::TemporalMeta;

    fn frame_ok(f: &WsFrame) -> bool {
        matches!(f, WsFrame::Response { ok: true, .. })
    }

    fn frame_data(f: &WsFrame) -> Value {
        match f {
            WsFrame::Response { payload, .. } => payload.clone().unwrap_or(Value::Null),
            _ => Value::Null,
        }
    }

    fn user_ctx(id: &str) -> UserContext {
        let mut ctx = UserContext::admin_fallback();
        ctx.user_id = id.to_string();
        ctx.role = UserRole::Employee;
        ctx
    }

    async fn put(db: &Path, agent: &str, subject: &str, pred: &str, obj: &str) -> String {
        let engine = SqliteMemoryEngine::new(db).unwrap();
        let entry = duduclaw_core::types::MemoryEntry {
            id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.to_string(),
            content: format!("{pred}: {obj}"),
            timestamp: Utc::now(),
            tags: vec!["user-profile".into()],
            embedding: None,
            layer: duduclaw_core::types::MemoryLayer::Semantic,
            importance: 6.0,
            access_count: 0,
            last_accessed: None,
            source_event: "user_profile".into(),
        };
        let meta = TemporalMeta {
            subject: Some(subject.to_string()),
            predicate: Some(pred.to_string()),
            object: Some(obj.to_string()),
            ..TemporalMeta::default()
        };
        engine
            .store_temporal(agent, entry, meta, duduclaw_memory::lineage::Provenance::test_only())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn list_is_isolated_between_users_and_forget_only_touches_own_rows() {
        let home = tempfile::tempdir().unwrap();
        let db = home.path().join("memory.db");
        let alice_row = put(&db, "agnes", "user:alice", "prefers", "烏龍茶").await;
        let alice_other_agent = put(&db, "bert", "user:alice", "reply_style", "簡短").await;
        let bob_row = put(&db, "agnes", "user:bob", "prefers", "咖啡").await;
        let handler = MethodHandler::new(home.path().to_path_buf()).await;

        // Alice sees exactly her two rows, from both AI employees.
        let f = handler
            .handle_my_memory_rpc("me.memory.list", json!({}), &user_ctx("alice"))
            .await;
        assert!(frame_ok(&f), "{f:?}");
        let data = frame_data(&f);
        let ids: Vec<&str> = data["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids.len(), 2, "{data}");
        assert!(ids.contains(&alice_row.as_str()) && ids.contains(&alice_other_agent.as_str()));
        assert!(!ids.contains(&bob_row.as_str()));
        let item = &data["items"][0];
        for key in ["id", "agent", "text", "created_at", "source_kind", "explicit"] {
            assert!(item.get(key).is_some(), "missing {key}: {item}");
        }
        assert_eq!(item["source_kind"], "profile");
        assert_eq!(item["explicit"], false);

        // Bob only sees his.
        let f = handler
            .handle_my_memory_rpc("me.memory.list", json!({ "limit": 50 }), &user_ctx("bob"))
            .await;
        let data = frame_data(&f);
        assert_eq!(data["items"].as_array().unwrap().len(), 1);
        assert_eq!(data["items"][0]["id"], bob_row);

        // Pagination: one per page.
        let f = handler
            .handle_my_memory_rpc("me.memory.list", json!({ "limit": 1 }), &user_ctx("alice"))
            .await;
        let data = frame_data(&f);
        let cursor = data["next_cursor"].as_str().unwrap().to_string();
        let f = handler
            .handle_my_memory_rpc(
                "me.memory.list",
                json!({ "limit": 1, "cursor": cursor }),
                &user_ctx("alice"),
            )
            .await;
        let data2 = frame_data(&f);
        assert_eq!(data2["items"].as_array().unwrap().len(), 1);
        assert_ne!(data2["items"][0]["id"], data["items"][0]["id"]);
        assert!(data2["next_cursor"].is_null());

        // Alice cannot forget Bob's row — same answer as a missing id.
        let f = handler
            .handle_my_memory_rpc("me.memory.forget", json!({ "id": bob_row }), &user_ctx("alice"))
            .await;
        assert!(!frame_ok(&f));
        let engine = SqliteMemoryEngine::new(&db).unwrap();
        assert!(engine.get_by_id("agnes", &bob_row).await.unwrap().is_some(), "bob's row intact");

        // Alice forgets her own row: gone, audited without content, and the
        // same triple is now refused for automatic re-learning.
        let f = handler
            .handle_my_memory_rpc("me.memory.forget", json!({ "id": alice_row }), &user_ctx("alice"))
            .await;
        assert!(frame_ok(&f), "{f:?}");
        assert!(engine.get_by_id("agnes", &alice_row).await.unwrap().is_none());
        let audit = std::fs::read_to_string(home.path().join("security_audit.jsonl")).unwrap_or_default();
        assert!(audit.contains("memory_self_forgotten"), "{audit}");
        assert!(!audit.contains("烏龍茶"), "audit carries no content");
        let gate = crate::memory_privacy::PrivacyGate::for_home(home.path());
        assert_eq!(
            gate.check(&crate::memory_privacy::FactParts {
                content: "prefers: 烏龍茶",
                subject: Some("user:alice"),
                predicate: Some("prefers"),
                object: Some("烏龍茶"),
            }),
            crate::memory_privacy::Verdict::Forgotten
        );

        // A pseudo-user cannot use the page.
        let f = handler
            .handle_my_memory_rpc("me.memory.list", json!({}), &user_ctx("system"))
            .await;
        assert!(!frame_ok(&f));
    }

    #[test]
    fn methods_have_an_explicit_role_and_cursor_parsing_is_strict() {
        assert_eq!(minimum_role("me.memory.list"), Some(UserRole::Employee));
        assert_eq!(minimum_role("me.memory.forget"), Some(UserRole::Employee));
        assert_eq!(minimum_role("me.memory.purge_all"), None);
        assert!(decode_cursor("2026-11-08T00:00:00+00:00|abc").is_some());
        assert!(decode_cursor("no-separator").is_none());
        assert!(decode_cursor("|abc").is_none());
    }
}
