//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

impl MethodHandler {
    /// WP14-T14.7: decide a pending approval from the dashboard ("就地同意/退回").
    /// Board-only kinds (WP17 StrategicPlan/AgentHire) require admin — the
    /// dashboard's fail-closed stand-in for board rights until a board-user model
    /// lands. Records `dashboard:<user_id>` as the decider for audit.
    // ── Agent Mail (P2-d) — 信箱 RPC ────────────────────────────────────
    //
    // Six read/act RPCs over `crate::mail`. The one thing worth calling out:
    // `mail.decide` does NOT own a decision store. It looks the draft's
    // `approval_id` up in the same `ApprovalBroker` the approval centre uses
    // and calls the same `decide`, so a mail confirmed from the 信箱 page, the
    // approval inbox, or the buttons pushed to a chat channel all land in one
    // auditable row — and the actual transmission still only ever happens in
    // `mail_worker::settle_outbox`, never here.

    /// B2a: when the mailbox's owning employee reads Gmail as a delegated
    /// colleague, the mail is that person's — only an admin and that person
    /// may open the mail pages. A `home` mailbox keeps the Manager gate.
    pub(crate) async fn mailbox_viewer_gate(&self, ctx: &UserContext) -> Result<(), String> {
        self.mailbox_access_gate(ctx, false).await
    }

    pub(crate) async fn mailbox_reader_gate(&self, ctx: &UserContext) -> Result<crate::google_identity::mailbox::Reader, String> {
        use crate::google_identity::mailbox::Reader;
        if ctx.is_admin() { return Ok(Reader::Admin); }
        let people = self.people_for_mail().await;
        let email = people.by_id(&ctx.user_id).filter(|p| p.active).map(|p| p.email);
        if ctx.role.at_least(UserRole::Manager) {
            return Ok(Reader::Manager { email });
        }
        email.map(|email| Reader::Person { email })
            .ok_or_else(|| "無法確認你的有效帳號，請重新登入或聯絡管理員。".into())
    }

    async fn mailbox_access_gate(&self, ctx: &UserContext, read_only: bool) -> Result<(), String> {
        let cfg = crate::mail::MailConfig::from_home(&self.home_dir);
        if ctx.is_admin() {
            return Ok(());
        }
        let mut accounts = crate::google_identity::mailbox::accounts(&self.home_dir)?;
        let owner = crate::mail_worker::resolve_owner_agent(&self.home_dir, &self.registry, &cfg).await;
        if cfg.gmail_enabled {
            match duduclaw_core::google_identity::effective_for(&self.home_dir, owner.as_deref()).effective {
                crate::google_identity::Effective::Home => {}
                crate::google_identity::Effective::Delegate { email } => accounts.push(email),
                _ => return Err("目前無法確認這個信箱所屬的 Google 帳號，只有管理員可以查看。".into()),
            }
        }
        if accounts.is_empty() {
            return acl::require_role(ctx, UserRole::Manager);
        }
        // Re-read the person: a cached dashboard context must not keep a
        // suspended account or a former email able to open personal mail.
        let people = self.people_for_mail().await;
        let Some(person) = people.by_id(&ctx.user_id).filter(|p| p.active) else {
            return Err("無法確認你的有效帳號，請重新登入或聯絡管理員。".into());
        };
        if accounts.iter().any(|email| !person.email.trim().eq_ignore_ascii_case(email)) {
            return Err("這個信箱包含個人的 Google 信件，只有帳號本人和管理員看得到；混合多個帳號時只限管理員。".into());
        }
        if read_only { Ok(()) } else { acl::require_role(ctx, UserRole::Manager) }
    }

    async fn people_for_mail(&self) -> Box<dyn crate::google_identity::requester_gate::People> {
        let users = self.user_db.read().await.clone();
        crate::google_identity::requester_gate::people_for(&self.home_dir, users.as_ref())
    }

    pub(crate) async fn handle_mail_status(&self) -> WsFrame {
        let home = self.home_dir.clone();
        let payload = tokio::task::spawn_blocking(move || {
            let cfg = crate::mail::MailConfig::from_home(&home);
            let smtp = crate::mail_worker::resolve_smtp_config(&home);
            json!({
                "enabled": cfg.enabled,
                "auto_trigger": cfg.auto_trigger,
                "gmail_enabled": cfg.gmail_enabled,
                "dropfolder_enabled": cfg.dropfolder_enabled,
                "poll_interval_secs": cfg.poll_interval_secs,
                "default_agent": cfg.default_agent,
                // Never the credentials — only whether a send is possible at
                // all, so the page can say "approved mail cannot go out yet".
                "smtp_configured": smtp.is_some(),
                "sender_allowlist_count": cfg.allowed_senders.len(),
                "recipient_allowlist_count": cfg.allowed_recipients.len(),
                "inbound_dir": crate::mail::inbound_dir(&home).to_string_lossy(),
            })
        })
        .await;
        match payload {
            Ok(v) => WsFrame::ok_response("", v),
            Err(e) => WsFrame::error_response("", &format!("mail status: {e}")),
        }
    }

    pub(crate) async fn handle_mail_list(&self, params: Value, reader: crate::google_identity::mailbox::Reader) -> WsFrame {
        let agent = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(a) = agent.as_deref()
            && !is_valid_agent_id(a)
        {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        let include_archived = params
            .get("include_archived")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(crate::mail::DEFAULT_QUERY_LIMIT);

        let home = self.home_dir.clone();
        let rows = tokio::task::spawn_blocking(move || {
            crate::mail::read_mailbox(&home).inbox.into_iter()
                .filter(|m| reader.allows(m))
                .filter(|m| include_archived || !m.archived)
                .filter(|m| agent.as_deref().is_none_or(|a| m.agent_id == a))
                .take(limit.clamp(1, crate::mail::MAX_QUERY_LIMIT))
                .collect::<Vec<_>>()
        })
        .await;
        match rows {
            Ok(items) => {
                // The list carries a snippet, not the body: a long thread must
                // not be paid for on every page render, and the full text is
                // one `mail.read` away.
                let messages: Vec<Value> = items
                    .iter()
                    .map(|m| {
                        json!({
                            "mail_id": m.mail_id,
                            "agent_id": m.agent_id,
                            "from": m.from,
                            "subject": m.subject,
                            "snippet": duduclaw_core::truncate_chars(&m.body, 160),
                            "received_at": m.received_at,
                            "source": m.source,
                            "read": m.read,
                            "archived": m.archived,
                            "handled": m.triggered,
                            "flagged": m.suspicious,
                            "risk_score": m.risk_score,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "count": messages.len(), "messages": messages }))
            }
            Err(e) => WsFrame::error_response("", &format!("mail list: {e}")),
        }
    }

    pub(crate) async fn handle_mail_read(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let reader = match self.mailbox_reader_gate(ctx).await {
            Ok(reader) => reader,
            Err(e) => return WsFrame::error_response("", &e),
        };
        let Some(mail_id) = params
            .get("mail_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return WsFrame::error_response("", "mail_id is required");
        };
        let home = self.home_dir.clone();
        let by = format!("dashboard:{}", ctx.user_id);
        let result = tokio::task::spawn_blocking(move || {
            let item = crate::mail::get_inbox_item(&home, &mail_id)?;
            if !reader.allows(&item) { return None; }
            crate::mail::mark_read(&home, &mail_id, &by);
            Some(item)
        })
        .await;
        match result {
            Ok(Some(m)) => WsFrame::ok_response(
                "",
                json!({
                    "mail_id": m.mail_id,
                    "agent_id": m.agent_id,
                    "from": m.from,
                    "subject": m.subject,
                    "body": m.body,
                    "received_at": m.received_at,
                    "source": m.source,
                    "read": true,
                    "archived": m.archived,
                    "handled": m.triggered,
                    "flagged": m.suspicious,
                    "risk_score": m.risk_score,
                }),
            ),
            Ok(None) => WsFrame::error_response("", "mail not found"),
            Err(e) => WsFrame::error_response("", &format!("mail read: {e}")),
        }
    }

    pub(crate) async fn handle_mail_archive(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(mail_id) = params
            .get("mail_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return WsFrame::error_response("", "mail_id is required");
        };
        let home = self.home_dir.clone();
        let by = format!("dashboard:{}", ctx.user_id);
        let ok =
            tokio::task::spawn_blocking(move || crate::mail::mark_archived(&home, &mail_id, &by))
                .await;
        match ok {
            Ok(true) => WsFrame::ok_response("", json!({ "ok": true })),
            Ok(false) => WsFrame::error_response("", "mail not found"),
            Err(e) => WsFrame::error_response("", &format!("mail archive: {e}")),
        }
    }

    pub(crate) async fn handle_mail_outbox(&self, params: Value) -> WsFrame {
        let agent = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if let Some(a) = agent.as_deref()
            && !is_valid_agent_id(a)
        {
            return WsFrame::error_response("", "Invalid agent_id format");
        }
        // An unrecognised status filter shows everything rather than silently
        // showing nothing — an empty list must mean "no such mail", never
        // "your filter was a typo".
        let status = match params.get("status").and_then(|v| v.as_str()) {
            Some("pending") => Some(crate::mail::OutboxStatus::Pending),
            Some("sent") => Some(crate::mail::OutboxStatus::Sent),
            Some("rejected") => Some(crate::mail::OutboxStatus::Rejected),
            Some("failed") => Some(crate::mail::OutboxStatus::Failed),
            _ => None,
        };
        let limit = params
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(crate::mail::DEFAULT_QUERY_LIMIT);

        let home = self.home_dir.clone();
        let rows = tokio::task::spawn_blocking(move || {
            crate::mail::list_outbox(&home, agent.as_deref(), status, limit)
        })
        .await;
        match rows {
            Ok(items) => {
                let drafts: Vec<Value> = items
                    .iter()
                    .map(|m| {
                        json!({
                            "mail_id": m.mail_id,
                            "agent_id": m.agent_id,
                            "to": m.to,
                            "subject": m.subject,
                            "body": m.body,
                            "created_at": m.created_at,
                            "status": m.status.as_str(),
                            "approval_id": m.approval_id,
                            "in_reply_to": m.in_reply_to,
                            "note": m.note,
                            "settled_at": m.settled_at,
                            // WP-7G: the decider's own free-text reason/comment,
                            // distinct from `note` (the fixed system copy the
                            // worker writes when it settles the draft).
                            "decision_note": m.decision_note,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "count": drafts.len(), "drafts": drafts }))
            }
            Err(e) => WsFrame::error_response("", &format!("mail outbox: {e}")),
        }
    }

    pub(crate) async fn handle_mail_decide(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let Some(mail_id) = params
            .get("mail_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
        else {
            return WsFrame::error_response("", "mail_id is required");
        };
        let Some(approve) = params.get("approve").and_then(|v| v.as_bool()) else {
            // Deliberately not defaulted: "approve" is a send decision, and a
            // missing field must never read as either answer.
            return WsFrame::error_response("", "approve (true/false) is required");
        };
        // WP-7G: optional free-text reason/comment from the decider. Blank or
        // absent is fine — the fixed system copy remains the fallback.
        let note = params
            .get("note")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);

        let home = self.home_dir.clone();
        let lookup_id = mail_id.clone();
        let draft = tokio::task::spawn_blocking(move || {
            crate::mail::list_outbox(&home, None, None, crate::mail::MAX_QUERY_LIMIT)
                .into_iter()
                .find(|d| d.mail_id == lookup_id)
        })
        .await;
        let draft = match draft {
            Ok(Some(d)) => d,
            Ok(None) => return WsFrame::error_response("", "draft not found"),
            Err(e) => return WsFrame::error_response("", &format!("mail decide: {e}")),
        };
        if draft.status != crate::mail::OutboxStatus::Pending {
            return WsFrame::error_response(
                "",
                &format!(
                    "這封信已經是 {} 狀態，不能再改決定。",
                    draft.status.as_str()
                ),
            );
        }
        if draft.approval_id.is_empty() {
            return WsFrame::error_response("", "這封草稿沒有對應的審核紀錄，無法確認。");
        }

        let broker = match crate::approval::ApprovalBroker::open(&self.home_dir) {
            Ok(b) => b,
            Err(e) => return WsFrame::error_response("", &format!("open approvals: {e}")),
        };
        let approval_id = crate::approval::ApprovalId::from(draft.approval_id.clone());
        // Confirm the row really is the mail approval it claims to be before
        // deciding it — otherwise a crafted mail_id could be used to decide an
        // unrelated approval through this narrower gate.
        match broker.get(&approval_id).await {
            Ok(Some(rec)) if rec.action_kind == crate::mail::OUTBOUND_ACTION_KIND => {}
            Ok(Some(_)) => {
                return WsFrame::error_response("", "審核紀錄與這封信不相符，已拒絕操作。");
            }
            Ok(None) => return WsFrame::error_response("", "找不到對應的審核紀錄。"),
            Err(e) => return WsFrame::error_response("", &format!("approval lookup: {e}")),
        }

        let decided_by = format!("dashboard:{}", ctx.user_id);
        if let Err(e) = broker.decide(&approval_id, approve, &decided_by).await {
            return WsFrame::error_response("", &format!("decide: {e}"));
        }
        // WP-7G: the decider's own note, kept independent of (and never
        // overwritten by) whatever terminal settle row lands later.
        if let Some(n) = &note {
            crate::mail::record_decision_note(&self.home_dir, &mail_id, &draft.agent_id, n);
        }
        // A rejection has nothing left to transmit, so there is no reason to
        // wait for the worker's next tick — settle it now. This is also the
        // only place the operator's actual reason (vs. the fixed system
        // copy "已由人工拒絕，未寄出。") can reach the ledger's `note` field.
        if !approve {
            let settle_note = note
                .clone()
                .unwrap_or_else(|| "已由人工拒絕，未寄出。".to_string());
            crate::mail::record_outbox_settled(
                &self.home_dir,
                &mail_id,
                &draft.agent_id,
                crate::mail::OutboxStatus::Rejected,
                &settle_note,
            );
        }
        // The worker performs (or refuses) the transmission on its next tick.
        // Saying "已寄出" here would be exactly the kind of self-reported
        // completion this project treats as a bug.
        WsFrame::ok_response(
            "",
            json!({
                "ok": true,
                "mail_id": mail_id,
                "approved": approve,
                "state": if approve { "approved_queued" } else { "rejected" },
                "note": note,
            }),
        )
    }

    pub(crate) fn required_approval_for_decision(
        lookup: Result<Option<crate::approval::ApprovalRecord>, String>,
    ) -> Result<crate::approval::ApprovalRecord, WsFrame> {
        match lookup {
            Ok(Some(rec)) => Ok(rec),
            Ok(None) => Err(WsFrame::error_response("", "approval not found")),
            Err(e) => Err(WsFrame::error_response("", &format!("read approval: {e}"))),
        }
    }
}
