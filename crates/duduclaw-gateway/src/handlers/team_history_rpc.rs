//! Dedicated read-only oversight RPCs; ordinary agent access is unchanged.
use super::*;
use crate::team_history::{
    audit,
    classify::HistoryClass,
    limits,
    mask::{self, Part, Transcript},
    reader::{self, ListFilter},
};
use crate::team_oversight::{self, OversightDenied, TeamOversight};
use std::collections::BTreeMap;

fn error(code: &str, message: &str, retry: Option<u64>) -> WsFrame {
    WsFrame::Response {
        id: String::new(),
        ok: false,
        payload: None,
        error: Some(json!({"code":code,"message":message,"retry_after_secs":retry})),
    }
}

fn optional_text<'a>(params: &'a Value, key: &str, max: usize) -> Result<Option<&'a str>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s))
            if !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control) =>
        {
            Ok(Some(s))
        }
        _ => Err(format!("invalid {key}")),
    }
}

fn sender_labels(
    db: &UserDb,
    scope: &TeamOversight,
    class: &HistoryClass,
) -> Result<BTreeMap<String, String>, String> {
    let HistoryClass::External { channel } = class else {
        return Ok(BTreeMap::new());
    };
    let mut labels = BTreeMap::new();
    for report in scope.reports.values() {
        for identity in db.verified_channels_for_user(&report.user_id)? {
            if identity.verified && identity.channel == *channel {
                labels.insert(identity.channel_user_id, report.display_name.clone());
            }
        }
    }
    Ok(labels)
}

fn report_json(scope: &TeamOversight, class: &HistoryClass) -> Value {
    match class {
        HistoryClass::ReportOwnWebchat { report } => scope
            .reports
            .get(report)
            .map(|r| json!({"user_id":r.user_id,"display_name":r.display_name}))
            .unwrap_or(Value::Null),
        _ => Value::Null,
    }
}
fn channel(class: &HistoryClass) -> Option<&str> {
    if let HistoryClass::External { channel } = class {
        Some(channel)
    } else {
        None
    }
}
fn title(transcript: &Transcript) -> String {
    let mut text = String::new();
    for message in &transcript.messages {
        for part in &message.parts {
            match part {
                Part::Text { v } => text.push_str(v),
                Part::Mask { cat, n } => text.push_str(&format!("〔{cat} {n}〕")),
                Part::Attachment { .. } => text.push_str("〔附件〕"),
            }
        }
    }
    text.chars().take(120).collect()
}

pub(super) fn viewed_summary(
    details: &Value,
    viewer: &str,
    agent: &str,
    report: Option<&str>,
) -> String {
    let messages = details.get("messages").and_then(Value::as_u64).unwrap_or(0);
    if details.get("class").and_then(Value::as_str) == Some("external") {
        let channel = details
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("外部")
            .to_uppercase();
        let masked: u64 = details
            .get("masked")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|m| m.values())
            .filter_map(Value::as_u64)
            .sum();
        format!("{viewer}檢視了〔{agent}〕與{channel}客戶的對話（{messages}則，遮蔽{masked}處）")
    } else {
        format!(
            "{viewer}檢視了〔{agent}〕與{}的WebChat對話（{messages}則）",
            report.unwrap_or("自己")
        )
    }
}

impl MethodHandler {
    fn history_denied(
        &self,
        ctx: &UserContext,
        agent: &str,
        reason: &str,
        code: &str,
        message: &str,
        retry: Option<u64>,
    ) -> WsFrame {
        if audit::write(
            &self.home_dir,
            "team_history_denied",
            &ctx.user_id,
            agent,
            json!({"viewer":ctx.user_id,"agent":agent,"reason":reason}),
            Some(format!("{agent}:{reason}")),
        )
        .is_err()
        {
            return error("audit_unavailable", "audit unavailable", None);
        }
        error(code, message, retry)
    }

    fn history_gate_error(
        &self,
        ctx: &UserContext,
        agent: &str,
        denial: OversightDenied,
    ) -> WsFrame {
        let (reason, code) = match denial {
            OversightDenied::SettingOff => ("setting_off", "team_history_off"),
            OversightDenied::ReadError => ("read_error", "audit_unavailable"),
            _ => ("not_in_team", "permission_denied"),
        };
        self.history_denied(ctx, agent, reason, code, "permission denied", None)
    }

    fn history_recheck(
        &self,
        db: &UserDb,
        ctx: &UserContext,
        scope: &TeamOversight,
    ) -> Result<(), String> {
        let fresh = team_oversight::can_view_team_history(&self.home_dir, db, ctx, &scope.agent)
            .map_err(|_| "permission denied")?;
        if fresh.since != scope.since
            || fresh.generation != scope.generation
            || fresh.reports != scope.reports
        {
            return Err("permission changed".into());
        }
        Ok(())
    }

    pub(crate) async fn handle_team_history_list(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let agent = params.get("agent_id").and_then(Value::as_str).unwrap_or("");
        if !is_valid_agent_id(agent) {
            return error("invalid_params", "agent_id is required", None);
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return error("read_error", "user database unavailable", None);
        };
        let scope = match team_oversight::can_view_team_history(&self.home_dir, &db, ctx, agent) {
            Ok(s) => s,
            Err(e) => return self.history_gate_error(ctx, agent, e),
        };
        if let Err(retry) = limits::take(&self.home_dir, &ctx.user_id, false) {
            return self.history_denied(
                ctx,
                agent,
                "rate_limited",
                "rate_limited",
                "rate limited",
                Some(retry),
            );
        }
        let limit = match params.get("limit") {
            None => 50,
            Some(v) => match v.as_u64() {
                Some(n) if n > 0 => n.min(200) as usize,
                _ => return error("invalid_params", "invalid limit", None),
            },
        };
        let parsed = (|| {
            Ok::<_, String>((
                optional_text(&params, "report_user_id", 128)?,
                optional_text(&params, "class", 32)?,
                optional_text(&params, "before", 4096)?,
            ))
        })();
        let (report, class, before) = match parsed {
            Ok(p) => p,
            Err(e) => return error("invalid_params", &e, None),
        };
        if class.is_some_and(|c| !matches!(c, "own" | "report_webchat" | "external")) {
            return error("invalid_params", "invalid class", None);
        }
        let filter = ListFilter {
            report: report.map(str::to_owned),
            class: class.map(str::to_owned),
        };
        if filter
            .report
            .as_ref()
            .is_some_and(|r| !scope.reports.contains_key(r))
        {
            return self.history_denied(
                ctx,
                agent,
                "not_in_team",
                "permission_denied",
                "permission denied",
                None,
            );
        }
        let Some(rc) = self.reply_ctx.read().await.clone() else {
            return error("read_error", "session store unavailable", None);
        };
        let page = match reader::list(&rc.session_manager, &scope, &filter, limit, before).await {
            Ok(p) => p,
            Err(_) => {
                return self.history_denied(
                    ctx,
                    agent,
                    "read_error",
                    "read_error",
                    "history unavailable",
                    None,
                );
            }
        };
        let mut sessions = Vec::new();
        let mut ner = true;
        for session in page.sessions {
            let external = matches!(session.class, HistoryClass::External { .. });
            let report = report_json(&scope, &session.class);
            let class = reader::class_name(&session.class);
            let ch = channel(&session.class).map(str::to_owned);
            let rendered = if external {
                mask::external(
                    self.home_dir.clone(),
                    session.rows,
                    BTreeMap::new(),
                    agent.into(),
                )
                .await
            } else {
                mask::full_transcript(&session.rows, "", agent)
            };
            let title = match rendered {
                Ok(t) => {
                    if external {
                        ner &= t.ner;
                    }
                    title(&t)
                }
                Err(_) => {
                    ner = false;
                    format!("{} 對話", ch.as_deref().unwrap_or("外部"))
                }
            };
            sessions.push(json!({"session_id":session.id,"class":class,"channel":ch,"report":report,"title":title,"title_masked":external,"last_active":session.last_active,"turns":session.turns}));
        }
        if self.history_recheck(&db, ctx, &scope).is_err() {
            return self.history_denied(
                ctx,
                agent,
                "not_in_team",
                "permission_denied",
                "permission denied",
                None,
            );
        }
        let dedup =
            serde_json::to_string(&(agent, &filter.report, &filter.class)).unwrap_or_default();
        if audit::write(&self.home_dir,"team_history_listed",&ctx.user_id,agent,json!({"viewer":ctx.user_id,"viewer_role":ctx.role,"agent":agent,"class":filter.class,"report":filter.report,"sessions":sessions.len(),"since":scope.since,"generation":scope.generation}),Some(dedup)).is_err(){return error("audit_unavailable","audit unavailable",None)}
        WsFrame::ok_response(
            "",
            json!({"sessions":sessions,"ner":ner,"since":scope.since,"next_before":page.next_before,"read_only":true}),
        )
    }

    pub(crate) async fn handle_team_history_get(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let id = params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if id.is_empty() || id.len() > 1024 {
            return error("invalid_params", "session_id is required", None);
        }
        let Some(rc) = self.reply_ctx.read().await.clone() else {
            return error("read_error", "session store unavailable", None);
        };
        let Some(db) = self.user_db.read().await.clone() else {
            return error("read_error", "user database unavailable", None);
        };
        match team_oversight::observe_window(&self.home_dir, &db, "gateway") {
            Ok(window) if !window.enabled => {
                return self.history_gate_error(ctx, "", OversightDenied::SettingOff);
            }
            Err(_) => return self.history_gate_error(ctx, "", OversightDenied::ReadError),
            _ => {}
        }
        // Limit before resolving an id: a hidden row and a missing row must
        // also behave identically when the viewer exhausts their quota.
        if let Err(retry) = limits::take(&self.home_dir, &ctx.user_id, true) {
            return self.history_denied(
                ctx,
                "",
                "rate_limited",
                "rate_limited",
                "rate limited",
                Some(retry),
            );
        }
        let agent = match rc.session_manager.session_agent(id).await {
            Ok(Some(a)) => a,
            _ => {
                return self.history_denied(
                    ctx,
                    "",
                    "hidden",
                    "not_found",
                    "session not found",
                    None,
                );
            }
        };
        let scope = match team_oversight::can_view_team_history(&self.home_dir, &db, ctx, &agent) {
            Ok(s) => s,
            Err(_) => {
                return self.history_denied(
                    ctx,
                    "",
                    "hidden",
                    "not_found",
                    "session not found",
                    None,
                );
            }
        };
        let session = match reader::get(&rc.session_manager, &scope, id).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                return self.history_denied(
                    ctx,
                    &agent,
                    "hidden",
                    "not_found",
                    "session not found",
                    None,
                );
            }
            Err(_) => {
                return self.history_denied(
                    ctx,
                    &agent,
                    "read_error",
                    "read_error",
                    "history unavailable",
                    None,
                );
            }
        };
        let limit = match params.get("limit") {
            None => 200,
            Some(v) => match v.as_u64() {
                Some(n) if n > 0 => n.min(500) as usize,
                _ => return error("invalid_params", "invalid limit", None),
            },
        };
        let labels = match sender_labels(&db, &scope, &session.class) {
            Ok(l) => l,
            Err(_) => {
                return self.history_denied(
                    ctx,
                    &agent,
                    "read_error",
                    "read_error",
                    "history unavailable",
                    None,
                );
            }
        };
        let user_label = match &session.class {
            HistoryClass::ReportOwnWebchat { report } => scope
                .reports
                .get(report)
                .map(|r| r.display_name.as_str())
                .unwrap_or("職員"),
            _ => "你",
        };
        let label = self
            .registry
            .read()
            .await
            .get(&agent)
            .map(|a| a.config.agent.display_name.clone())
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| agent.clone());
        let rendered = if matches!(session.class, HistoryClass::External { .. }) {
            mask::external(self.home_dir.clone(), session.rows, labels, label.clone()).await
        } else {
            mask::full_transcript(&session.rows, user_label, &label)
        };
        let mut transcript = match rendered {
            Ok(t) => t,
            Err(_) => {
                return self.history_denied(
                    ctx,
                    &agent,
                    "masking_failed",
                    "masking_failed",
                    "masking failed",
                    None,
                );
            }
        };
        if self.history_recheck(&db, ctx, &scope).is_err() {
            return self.history_denied(
                ctx,
                &agent,
                "hidden",
                "not_found",
                "session not found",
                None,
            );
        }
        let digest = match audit::session_digest(&self.home_dir, id) {
            Ok(d) => d,
            Err(_) => return error("audit_unavailable", "audit unavailable", None),
        };
        if transcript.messages.len() > limit {
            transcript
                .messages
                .drain(..transcript.messages.len() - limit);
        }
        mask::recount(&mut transcript);
        let report = match &session.class {
            HistoryClass::ReportOwnWebchat { report } => scope.reports.get(report),
            _ => None,
        };
        if audit::write(&self.home_dir,"team_history_viewed",&ctx.user_id,&agent,json!({"viewer":ctx.user_id,"viewer_role":ctx.role,"agent":agent,"session":digest,"class":reader::class_name(&session.class),"channel":channel(&session.class),"report":report.map(|r|&r.user_id),"via_depth":report.map(|r|r.depth),"messages":transcript.messages.len(),"masked":transcript.masked_counts,"ner":transcript.ner,"since":scope.since,"generation":scope.generation}),None).is_err(){return error("audit_unavailable","audit unavailable",None)}
        WsFrame::ok_response(
            "",
            json!({"session_id":id,"agent_id":agent,"class":reader::class_name(&session.class),"read_only":true,"ner":transcript.ner,"since":scope.since,"messages":transcript.messages,"masked_counts":transcript.masked_counts}),
        )
    }

    pub(crate) async fn handle_org_history_settings(
        &self,
        params: Value,
        ctx: &UserContext,
        update: bool,
    ) -> WsFrame {
        let Some(db) = self.user_db.read().await.clone() else {
            return error("read_error", "user database unavailable", None);
        };
        if !db
            .live_user_context(&ctx.user_id)
            .ok()
            .flatten()
            .is_some_and(|u| u.is_admin() && !u.requires_password_change())
        {
            return error("permission_denied", "permission denied", None);
        }
        if update {
            use super::config_commit::{
                bool_param, commit_table_locked_with_guard, content_hash, table_at_mut,
            };
            let enabled = match bool_param(&params, "manager_team_history") {
                Ok(Some(b)) => b,
                _ => {
                    return error(
                        "invalid_params",
                        "manager_team_history must be a boolean",
                        None,
                    );
                }
            };
            // Observe the old value before replacing it, including an off
            // period whose transition audit previously failed.
            if team_oversight::observe_window(&self.home_dir, &db, &ctx.user_id).is_err() {
                return error("audit_unavailable", "audit unavailable", None);
            }
            let path = self.home_dir.join("config.toml");
            let raw = match std::fs::read_to_string(&path) {
                Ok(r) => r,
                Err(_) => return error("read_error", "config unavailable", None),
            };
            let mut table = match raw.parse::<toml::Table>() {
                Ok(t) => t,
                Err(_) => return error("invalid_config", "config invalid", None),
            };
            let before = super::config_commit::toml_at_json(&table, "people.manager_team_history");
            match table_at_mut(&mut table, &["people"]) {
                Ok(t) => {
                    t.insert("manager_team_history".into(), toml::Value::Boolean(enabled));
                }
                Err(_) => return error("invalid_config", "people must be a table", None),
            }
            let guard_db = db.clone();
            let user = ctx.user_id.clone();
            if let Err(e) =
                commit_table_locked_with_guard(&path, content_hash(&raw), &table, move || {
                    if guard_db
                        .live_user_context(&user)?
                        .is_some_and(|u| u.is_admin() && !u.requires_password_change())
                    {
                        Ok(())
                    } else {
                        Err("permission denied".into())
                    }
                })
                .await
            {
                return error("config_write_failed", &e, None);
            }
            if duduclaw_security::audit::try_append_audit_event(&self.home_dir,&duduclaw_security::audit::AuditEvent::new(
                "config_protected_key_changed","",duduclaw_security::audit::Severity::Warning,
                json!({"key":"people.manager_team_history","before":before,"after":enabled,"user_id":ctx.user_id,"source":"users.org_settings.update"})
            )).is_err(){return error("audit_unavailable","audit unavailable",None)}
        }
        match team_oversight::observe_window(&self.home_dir, &db, &ctx.user_id) {
            Ok(window) => WsFrame::ok_response(
                "",
                json!({"manager_team_history":window.enabled,"since":window.since_micros.and_then(DateTime::from_timestamp_micros)}),
            ),
            Err(_) => error("audit_unavailable", "audit unavailable", None),
        }
    }

    pub(crate) async fn handle_history_resolve_digest(&self, params: Value) -> WsFrame {
        let digest = params.get("digest").and_then(Value::as_str).unwrap_or("");
        if digest.len() != 16 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return error("invalid_params", "invalid digest", None);
        }
        let Some(rc) = self.reply_ctx.read().await.clone() else {
            return error("read_error", "session store unavailable", None);
        };
        let conn = rc.session_manager.acquire().await;
        let mut stmt = match conn.prepare("SELECT id FROM sessions") {
            Ok(s) => s,
            Err(_) => return error("read_error", "session store unavailable", None),
        };
        let rows = match stmt.query_map([], |r| r.get::<_, String>(0)) {
            Ok(r) => r,
            Err(_) => return error("read_error", "session store unavailable", None),
        };
        for row in rows {
            let Ok(id) = row else {
                return error("read_error", "session store unavailable", None);
            };
            match audit::session_digest(&self.home_dir, &id) {
                Ok(d) if d == digest => return WsFrame::ok_response("", json!({"session_id":id})),
                Err(_) => return error("audit_unavailable", "audit unavailable", None),
                _ => {}
            }
        }
        error("not_found", "session not found", None)
    }
}
