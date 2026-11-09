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
use crate::history_privacy::handles;
use std::collections::BTreeMap;

/// What `users.team_history.get`'s authorization established (RM-173 shares
/// it with `users.team_history.restore`).
struct TeamHistoryAccess {
    db: Arc<UserDb>,
    /// Resolved raw session id.
    id: String,
    agent: String,
    scope: TeamOversight,
    session: reader::ReadSession,
}

/// RM-173: the earliest team-history boundary an Admin may choose.
const HISTORY_SINCE_FLOOR: &str = "2020-01-01T00:00:00Z";
const HISTORY_SINCE_STEP_UP_REQUIRED: &str =
    "提早團隊紀錄起算時間會讓主管看到更早的對話，請輸入您目前的登入密碼以確認。";

/// RFC 3339 time or `YYYY-MM-DD` (00:00 UTC), between the floor and `now`.
fn parse_history_since(raw: &str, now: DateTime<chrono::Utc>) -> Result<i64, &'static str> {
    let at = if let Ok(t) = DateTime::parse_from_rfc3339(raw.trim()) {
        t.with_timezone(&chrono::Utc)
    } else if let Ok(d) = chrono::NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d") {
        d.and_hms_opt(0, 0, 0).ok_or("invalid team_history_since")?.and_utc()
    } else {
        return Err("team_history_since must be an RFC 3339 time or a YYYY-MM-DD date");
    };
    let floor = DateTime::parse_from_rfc3339(HISTORY_SINCE_FLOOR)
        .map_err(|_| "invalid team_history_since")?
        .with_timezone(&chrono::Utc);
    if at < floor || at > now {
        return Err("team_history_since must be between 2020-01-01 and now");
    }
    Ok(at.timestamp_micros())
}

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
        let before = match before {
            Some(value) => match handles::resolve_cursor(&self.home_dir,&ctx.user_id,value).await {
                Ok(raw)=>Some(raw),
                Err(_)=>return error("invalid_cursor","refresh history",None),
            },
            None=>None,
        };
        let page = match reader::list(&rc.session_manager, &scope, &filter, limit, before.as_deref()).await {
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
            let handle=match handles::issue(&self.home_dir,&ctx.user_id,&session.id) {
                Ok(handle)=>handle,Err(_)=>return error("audit_unavailable","history unavailable",None),
            };
            sessions.push(json!({"session_id":handle,"class":class,"channel":ch,"report":report,"title":title,"title_masked":external,"last_active":session.last_active,"turns":session.turns,"archived":session.archived}));
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
        let next_before=match page.next_before {
            Some(raw)=>match handles::issue_cursor(&self.home_dir,&ctx.user_id,&raw) {
                Ok(handle)=>Some(handle),Err(_)=>return error("audit_unavailable","history unavailable",None),
            },
            None=>None,
        };
        WsFrame::ok_response(
            "",
            json!({"sessions":sessions,"ner":ner,"since":scope.since,"next_before":next_before,"read_only":true}),
        )
    }

    /// The complete `users.team_history.get` authorization: setting/edition
    /// window, rate limit, handle resolution, live manager scope over the
    /// owning agent and the stored-origin / activation-window read. Shared
    /// verbatim with `users.team_history.restore` (RM-173) so "may restore"
    /// can never drift from "may read".
    async fn team_history_session_access(
        &self,
        params: &Value,
        ctx: &UserContext,
    ) -> Result<TeamHistoryAccess, WsFrame> {
        let id = params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if id.is_empty() || id.len() > 1024 {
            return Err(error("invalid_params", "session_id is required", None));
        }
        let Some(rc) = self.reply_ctx.read().await.clone() else {
            return Err(error("read_error", "session store unavailable", None));
        };
        let Some(db) = self.user_db.read().await.clone() else {
            return Err(error("read_error", "user database unavailable", None));
        };
        match team_oversight::observe_window(&self.home_dir, &db, "gateway") {
            Ok(window) if !window.enabled => {
                return Err(self.history_gate_error(ctx, "", OversightDenied::SettingOff));
            }
            Err(_) => return Err(self.history_gate_error(ctx, "", OversightDenied::ReadError)),
            _ => {}
        }
        // Limit before resolving an id: a hidden row and a missing row must
        // also behave identically when the viewer exhausts their quota.
        if let Err(retry) = limits::take(&self.home_dir, &ctx.user_id, true) {
            return Err(self.history_denied(
                ctx,
                "",
                "rate_limited",
                "rate_limited",
                "rate limited",
                Some(retry),
            ));
        }
        let resolved=match handles::resolve(&self.home_dir,&ctx.user_id,id).await {
            Ok(raw)=>raw,
            Err(_)=>return Err(self.history_denied(ctx,"","hidden","not_found","session not found",None)),
        };
        let agent = match rc.session_manager.session_agent(&resolved).await {
            Ok(Some(a)) => a,
            _ => {
                return Err(self.history_denied(
                    ctx,
                    "",
                    "hidden",
                    "not_found",
                    "session not found",
                    None,
                ));
            }
        };
        let scope = match team_oversight::can_view_team_history(&self.home_dir, &db, ctx, &agent) {
            Ok(s) => s,
            Err(_) => {
                return Err(self.history_denied(
                    ctx,
                    "",
                    "hidden",
                    "not_found",
                    "session not found",
                    None,
                ));
            }
        };
        let session = match reader::get(&rc.session_manager, &scope, &resolved).await {
            Ok(Some(s)) => s,
            Ok(None) => {
                return Err(self.history_denied(
                    ctx,
                    &agent,
                    "hidden",
                    "not_found",
                    "session not found",
                    None,
                ));
            }
            Err(_) => {
                return Err(self.history_denied(
                    ctx,
                    &agent,
                    "read_error",
                    "read_error",
                    "history unavailable",
                    None,
                ));
            }
        };
        Ok(TeamHistoryAccess { db, id: resolved, agent, scope, session })
    }

    pub(crate) async fn handle_team_history_get(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let TeamHistoryAccess { db, id: resolved, agent, scope, session } =
            match self.team_history_session_access(&params, ctx).await {
                Ok(access) => access,
                Err(frame) => return frame,
            };
        let id = resolved.as_str();
        let archived = session.archived;
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
        let handle=match handles::issue(&self.home_dir,&ctx.user_id,id) {
            Ok(handle)=>handle,Err(_)=>return error("audit_unavailable","history unavailable",None),
        };
        WsFrame::ok_response(
            "",
            json!({"session_id":handle,"agent_id":agent,"class":reader::class_name(&session.class),"read_only":true,"ner":transcript.ner,"since":scope.since,"messages":transcript.messages,"masked_counts":transcript.masked_counts,"archived":archived}),
        )
    }

    /// RM-173 `users.team_history.restore {session_id}` — clear `archived_at`
    /// of a conversation. Allowed for a live Admin (any existing session,
    /// including one the user cleared — `archive_reason` is audited), or for a
    /// manager exactly when `users.team_history.get` would release that
    /// session to them right now ([`Self::team_history_session_access`],
    /// including its rate limit, activation window and denial audit) — never
    /// a user-cleared one, refused like a missing session. Audited as
    /// `team_history_session_restored` (fail closed) before the write. An
    /// Admin restore keeps the sticky `user_cleared_at`, so a restored user
    /// clear still never appears in team history.
    pub(crate) async fn handle_team_history_restore(
        &self,
        params: Value,
        ctx: &UserContext,
    ) -> WsFrame {
        let requested = params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if requested.is_empty() || requested.len() > 1024 {
            return error("invalid_params", "session_id is required", None);
        }
        let Some(db) = self.user_db.read().await.clone() else {
            return error("read_error", "user database unavailable", None);
        };
        let Some(rc) = self.reply_ctx.read().await.clone() else {
            return error("read_error", "session store unavailable", None);
        };
        let admin = ctx.user_id != "system"
            && db
                .live_user_context(&ctx.user_id)
                .ok()
                .flatten()
                .is_some_and(|u| u.is_admin() && !u.requires_password_change());
        let (id, agent, via, scope) = if admin {
            let Ok(raw) = handles::resolve(&self.home_dir, &ctx.user_id, requested).await else {
                return error("not_found", "session not found", None);
            };
            let agent = match rc.session_manager.session_agent(&raw).await {
                Ok(Some(agent)) => agent,
                _ => return error("not_found", "session not found", None),
            };
            // v1.81 P1: another person's Personal employee's conversation is
            // restored by an Admin only through the audited break-glass
            // step-up (protected mode; a no-op elsewhere). The manager path
            // below never reaches a Personal employee (team oversight skips
            // them in SQL).
            let step_up = params.get(super::personal_data_gate::STEP_UP_FIELD)
                .and_then(|raw| serde_json::from_value::<[u8; 32]>(raw.clone()).ok());
            if let Err(e) = self.personal_data_gate(ctx, super::personal_data_gate::Target {
                method: "users.team_history.restore",
                category: crate::personal_visibility::DataCategory::Sessions,
                agent: &agent,
            }, step_up).await {
                return error("permission_denied", e, None);
            }
            (raw, agent, "admin", None)
        } else {
            match self.team_history_session_access(&params, ctx).await {
                Ok(access) => (access.id, access.agent, "team_history", Some(access.scope)),
                Err(frame) => return frame,
            }
        };
        let state = match rc.session_manager.archive_state(&id).await {
            Ok(a) => a,
            Err(_) => return error("read_error", "history unavailable", None),
        };
        let Some((archived_at, reason)) = state else {
            return WsFrame::ok_response("", json!({"session_id":requested,"restored":false}));
        };
        // RM-173: the sticky mark — survives a restore, so a once-cleared
        // conversation that was restored and re-archived still counts.
        let user_cleared = match rc.session_manager.user_cleared_at(&id).await {
            Ok(mark) => mark.is_some() || reason == crate::session::ARCHIVE_REASON_USER,
            Err(_) => return error("read_error", "history unavailable", None),
        };
        if let Some(scope) = &scope {
            // A conversation the user cleared is theirs alone: never restorable
            // through team history (the reader already hides it; this keeps
            // the refusal even if a clear races the read). Same shape as a
            // missing session.
            if user_cleared || self.history_recheck(&db, ctx, scope).is_err() {
                return self.history_denied(ctx, &agent, "hidden", "not_found", "session not found", None);
            }
        }
        let digest = match audit::session_digest(&self.home_dir, &id) {
            Ok(d) => d,
            Err(_) => return error("audit_unavailable", "audit unavailable", None),
        };
        if audit::write(
            &self.home_dir,
            "team_history_session_restored",
            &ctx.user_id,
            &agent,
            json!({"actor":ctx.user_id,"actor_role":ctx.role,"agent":agent,"session":digest,"archived_at":archived_at,"archive_reason":reason,"user_cleared":user_cleared,"via":via,"since":scope.as_ref().map(|s|s.since),"generation":scope.as_ref().map(|s|s.generation)}),
            None,
        )
        .is_err()
        {
            return error("audit_unavailable", "audit unavailable", None);
        }
        match rc.session_manager.restore_archived(&id).await {
            Ok(_) => WsFrame::ok_response("", json!({"session_id":requested,"restored":true})),
            Err(_) => error("write_failed", "restore failed", None),
        }
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
                toml_at_json, u64_param,
            };
            use crate::session_retention::{ARCHIVE_KEY, MAX_DAYS, PURGE_KEY};
            let enabled = match bool_param(&params, "manager_team_history") {
                Ok(v) => v,
                Err(_) => {
                    return error(
                        "invalid_params",
                        "manager_team_history must be a boolean",
                        None,
                    );
                }
            };
            // RM-173: conversation retention (`[sessions]`).
            let archive = match u64_param(&params, ARCHIVE_KEY, 0, MAX_DAYS) {
                Ok(v) => v,
                Err(e) => return error("invalid_params", &e, None),
            };
            let purge = match u64_param(&params, PURGE_KEY, 0, MAX_DAYS) {
                Ok(v) => v,
                Err(e) => return error("invalid_params", &e, None),
            };
            // RM-173: the team-history activation boundary.
            let since = match params.get("team_history_since") {
                None | Some(Value::Null) => None,
                Some(Value::String(s)) => match parse_history_since(s, chrono::Utc::now()) {
                    Ok(micros) => Some(micros),
                    Err(e) => return error("invalid_params", e, None),
                },
                Some(_) => {
                    return error("invalid_params", "team_history_since must be a date", None);
                }
            };
            if enabled.is_none() && archive.is_none() && purge.is_none() && since.is_none() {
                return error("invalid_params", "manager_team_history must be a boolean", None);
            }
            if enabled.is_some() && since.is_some() {
                // Re-enabling starts a fresh window at "now"; never combine.
                return error(
                    "invalid_params",
                    "change team_history_since separately from manager_team_history",
                    None,
                );
            }
            if (archive.is_some() || purge.is_some()) && since.is_some() {
                // F4: retention lives in config.toml, the boundary in the user
                // DB — two commits. Refuse the combination outright so a
                // failure of the second step can never leave a partial update.
                return error(
                    "invalid_params",
                    "change team_history_since separately from conversation retention",
                    None,
                );
            }
            if ctx.user_id == "system" && since.is_some() {
                // F3: the shared-token context never moves the boundary.
                return error("permission_denied", "permission denied", None);
            }

            // Verify a backdating step-up BEFORE anything is written, so a
            // refused step-up leaves every setting untouched.
            let mut since_step_up: Option<&'static str> = None;
            if let Some(new_since) = since {
                let window = match team_oversight::observe_window(&self.home_dir, &db, &ctx.user_id) {
                    Ok(w) => w,
                    Err(_) => return error("audit_unavailable", "audit unavailable", None),
                };
                if !window.enabled {
                    return error("team_history_off", "team history is off", None);
                }
                let Some(old) = window.since_micros else {
                    return error("audit_unavailable", "audit unavailable", None);
                };
                if new_since < old {
                    match self.history_since_step_up(&db, &params, ctx).await {
                        Ok(kind) => since_step_up = Some(kind),
                        Err(frame) => return frame,
                    }
                }
            }

            if enabled.is_some() || archive.is_some() || purge.is_some() {
                if enabled.is_some() {
                    // Observe the old value before replacing it, including an
                    // off period whose transition audit previously failed.
                    if team_oversight::observe_window(&self.home_dir, &db, &ctx.user_id).is_err() {
                        return error("audit_unavailable", "audit unavailable", None);
                    }
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
                let mut changes: Vec<(String, Value, Value)> = Vec::new();
                if let Some(enabled) = enabled {
                    let before = toml_at_json(&table, "people.manager_team_history");
                    match table_at_mut(&mut table, &["people"]) {
                        Ok(t) => {
                            t.insert("manager_team_history".into(), toml::Value::Boolean(enabled));
                        }
                        Err(_) => return error("invalid_config", "people must be a table", None),
                    }
                    changes.push(("people.manager_team_history".into(), before, json!(enabled)));
                }
                for (key, value) in [(ARCHIVE_KEY, archive), (PURGE_KEY, purge)] {
                    let Some(days) = value else { continue };
                    let path_key = format!("sessions.{key}");
                    let before = toml_at_json(&table, &path_key);
                    match table_at_mut(&mut table, &["sessions"]) {
                        Ok(t) => {
                            t.insert(key.into(), toml::Value::Integer(days as i64));
                        }
                        Err(_) => return error("invalid_config", "sessions must be a table", None),
                    }
                    changes.push((path_key, before, json!(days)));
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
                for (key, before, after) in changes {
                    if duduclaw_security::audit::try_append_audit_event(&self.home_dir,&duduclaw_security::audit::AuditEvent::new(
                        "config_protected_key_changed","",duduclaw_security::audit::Severity::Warning,
                        json!({"key":key,"before":before,"after":after,"user_id":ctx.user_id,"source":"users.org_settings.update"})
                    )).is_err(){return error("audit_unavailable","audit unavailable",None)}
                }
            }

            if let Some(new_since) = since {
                let home = self.home_dir.clone();
                let actor = ctx.user_id.clone();
                let step_up = since_step_up;
                // F3: the auth layer re-checks, inside its write transaction,
                // that `actor` is a live Admin (never the shared-token
                // `system` context) for either direction.
                let moved = db.set_team_history_since(&ctx.user_id, new_since, move |old, new| {
                    // A concurrent change may have moved the boundary since the
                    // step-up decision: never backdate without one.
                    if new < old && step_up.is_none() {
                        return Err("step-up required".into());
                    }
                    // This JSONL line is written before the user-DB commit. If
                    // the commit then fails, the log records a change that did
                    // not happen — the safe direction (never a silent change).
                    duduclaw_security::audit::try_append_audit_event(
                        &home,
                        &duduclaw_security::audit::AuditEvent::new(
                            "team_history_since_changed",
                            "",
                            duduclaw_security::audit::Severity::Warning,
                            json!({
                                "actor": actor,
                                "old": DateTime::from_timestamp_micros(old),
                                "new": DateTime::from_timestamp_micros(new),
                                "old_micros": old,
                                "new_micros": new,
                                "direction": if new < old { "earlier" } else { "later" },
                                "exposes_older_conversations": new < old,
                                "step_up": step_up,
                                "source": "users.org_settings.update",
                            }),
                        ),
                    )
                });
                match moved {
                    Ok(_) => {}
                    Err(e) if e == "team history is off" => {
                        return error("team_history_off", "team history is off", None);
                    }
                    Err(e) if e == "step-up required" => {
                        return error("step_up_required", HISTORY_SINCE_STEP_UP_REQUIRED, None);
                    }
                    Err(e) if e == "permission denied" => {
                        return error("permission_denied", "permission denied", None);
                    }
                    Err(_) => return error("audit_unavailable", "audit unavailable", None),
                }
            }
        }
        match team_oversight::observe_window(&self.home_dir, &db, &ctx.user_id) {
            Ok(window) => {
                let since = window.since_micros.and_then(DateTime::from_timestamp_micros);
                let retention = crate::session_retention::load(&self.home_dir);
                WsFrame::ok_response(
                    "",
                    json!({
                        "manager_team_history": window.enabled,
                        "since": since,
                        "team_history_since": since,
                        "team_history_since_floor": HISTORY_SINCE_FLOOR,
                        "archive_after_days": retention.archive_after_days,
                        "purge_after_days": retention.purge_after_days,
                    }),
                )
            }
            Err(_) => error("audit_unavailable", "audit unavailable", None),
        }
    }

    /// RM-173: moving the team-history boundary EARLIER exposes older
    /// conversations to managers — require the fresh admin step-up shared
    /// with RM-166 (own current password, plus TOTP / recovery code when MFA
    /// is active). Failed attempts are audited.
    async fn history_since_step_up(
        &self,
        db: &Arc<UserDb>,
        params: &Value,
        ctx: &UserContext,
    ) -> Result<&'static str, WsFrame> {
        use super::identity_grace_rpc::{MFA_CODE_REQUIRED, StepUpCopy, admin_step_up};
        if ctx.user_id == "system" {
            return Err(error("step_up_required", HISTORY_SINCE_STEP_UP_REQUIRED, None));
        }
        let Some(password) = params
            .get("password")
            .and_then(Value::as_str)
            .filter(|p| !p.is_empty() && p.len() <= 4096)
            .map(str::to_owned)
        else {
            return Err(error("step_up_required", HISTORY_SINCE_STEP_UP_REQUIRED, None));
        };
        if !crate::server::check_login_rate_limit(
            std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED),
            &ctx.email,
        ) {
            return Err(error("rate_limited", "嘗試次數過多，請稍後再試。", None));
        }
        let code = params
            .get("code")
            .and_then(Value::as_str)
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty() && c.len() <= 64);
        let copy = StepUpCopy {
            failed_password: "密碼不正確，未變更團隊紀錄起算時間。",
            failed_mfa: "驗證碼不正確，未變更團隊紀錄起算時間。",
        };
        match admin_step_up(&self.home_dir, db, ctx, password, code, copy).await {
            Ok(kind) => Ok(kind),
            Err((message, failed_step)) => {
                if let Some(step) = failed_step {
                    // Best-effort: a failed attempt is evidence, never a gate.
                    // Same two sinks as the identity-grace step-up flow.
                    let _ = db.log_action(
                        Some(&ctx.user_id),
                        "team_history.since_step_up_failed",
                        None,
                        Some(step),
                        None,
                    );
                    duduclaw_security::audit::append_audit_event(
                        &self.home_dir,
                        &duduclaw_security::audit::AuditEvent::new(
                            "team_history_since_step_up_failed",
                            "",
                            duduclaw_security::audit::Severity::Warning,
                            json!({"actor": ctx.user_id, "failed_step": step}),
                        ),
                    );
                }
                let code = if message == MFA_CODE_REQUIRED {
                    "mfa_code_required"
                } else if failed_step.is_some() {
                    "step_up_failed"
                } else {
                    "permission_denied"
                };
                Err(error(code, &message, None))
            }
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
