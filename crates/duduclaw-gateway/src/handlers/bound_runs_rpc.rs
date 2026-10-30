//! Session-run projections use stored conversation scope; dispatch/task runs
//! retain their separate task audience in runs_rpc.
use super::*;
use crate::history_privacy::{Class, Mode, handles, read};
impl MethodHandler {
    pub(super) async fn bound_runs_list(
        &self,
        ctx: &UserContext,
        agent: &str,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        crate::team_history::limits::take(&self.home_dir, &format!("bound:{}", ctx.user_id), false)
            .map_err(|_| "history rate limited")?;
        let home = self.home_dir.clone();
        let user = ctx.user_id.clone();
        let a = agent.to_owned();
        let permit = read::permit()?;
        let rows = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let c = read::open(&home)?;
            let sessions = read::list(&c, &user, &a, limit.max(1).min(500))?;
            let mut out = vec![];
            let mut bytes = 0usize;
            for m in sessions {
                let external = matches!(m.class, Class::External { .. });
                let channel = match m.class {
                    Class::External { channel } => channel,
                    _ => "webchat".into(),
                };
                let source = read::rows(&c, &m.id)?;
                for r in source {
                    bytes = bytes.saturating_add(r.message.content.len());
                    if out.len() >= 20_000 || bytes > read::MAX_BYTES {
                        return Err("history unavailable".into());
                    }
                    out.push((
                        RunMsgRow {
                            rowid: r.id,
                            session_id: m.id.clone(),
                            agent_id: a.clone(),
                            role: r.message.role,
                            ts: r.message.timestamp,
                            preview: if external {
                                "External conversation".into()
                            } else {
                                duduclaw_core::truncate_chars(
                                    &r.message.content,
                                    RUN_TEXT_PREVIEW_CHARS,
                                )
                            },
                        },
                        channel.clone(),
                        external,
                    ));
                }
            }
            Ok::<_, String>(out)
        })
        .await
        .map_err(|_| "history unavailable")??;
        let classes: HashMap<_, _> = rows
            .iter()
            .map(|(r, ch, e)| (r.session_id.clone(), (ch.clone(), *e)))
            .collect();
        let mut rows: Vec<_> = rows.into_iter().map(|(r, _, _)| r).collect();
        rows.sort_by(|a, b| (&a.session_id, a.rowid).cmp(&(&b.session_id, b.rowid)));
        let mut runs = fold_session_runs(&rows, Utc::now());
        apply_channel_run_failures(&self.home_dir, &mut runs);
        runs.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        runs.truncate(limit);
        let mut out = vec![];
        for mut r in runs {
            r.channel = classes[&r.session_id].0.clone();
            r.id = handles::issue_run(&self.home_dir, &ctx.user_id, &r.id)?;
            r.session_id = handles::issue(&self.home_dir, &ctx.user_id, &r.session_id)?;
            out.push(run_summary_to_json(&r, 0));
        }
        self.bound_history_recheck(ctx, agent, Mode::Filtered)?;
        Ok(out)
    }
    pub(super) async fn bound_run_get(
        &self,
        ctx: &UserContext,
        run_id: &str,
        agent: &str,
        class: Class,
    ) -> Result<Value, String> {
        crate::team_history::limits::take(&self.home_dir, &format!("bound:{}", ctx.user_id), true)
            .map_err(|_| "history rate limited")?;
        let (sid, row) = run_id.rsplit_once('#').ok_or("Run not found")?;
        let row: i64 = row.parse().map_err(|_| "Run not found")?;
        let home = self.home_dir.clone();
        let raw = sid.to_owned();
        let permit = read::permit()?;
        let rows = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            read::rows(&read::open(&home)?, &raw)
        })
        .await
        .map_err(|_| "history unavailable")??;
        let start = rows
            .iter()
            .position(|r| r.id == row && r.message.role == "user")
            .ok_or("Run not found")?;
        let tail = &rows[start..];
        let end = tail
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, r)| r.message.role == "user")
            .map(|(i, _)| i)
            .unwrap_or(tail.len());
        let source: Vec<_> = tail[..end]
            .iter()
            .map(|r| crate::session::SessionMessage {
                role: r.message.role.clone(),
                content: r.message.content.clone(),
                tokens: r.message.tokens,
                timestamp: r.message.timestamp.clone(),
            })
            .collect();
        let external = matches!(class, Class::External { .. });
        let channel = match class {
            Class::External { channel } => channel,
            _ => "webchat".into(),
        };
        let transcript = if external {
            crate::history_privacy::projection::project(
                self.home_dir.clone(),
                source,
                true,
                agent.into(),
            )
            .await?
        } else {
            crate::history_privacy::projection::own(source, agent)
        };
        let first = transcript.messages.first().ok_or("Run not found")?;
        let last = transcript.messages.iter().find(|r| r.role == "assistant");
        let events:Vec<_>=transcript.messages.iter().map(|m|json!({"kind":"text","role":m.role,"ts":m.timestamp,"preview":crate::history_privacy::plain_parts(&m.parts)})).collect();
        let failed_end = crate::run_steps::shared_store(&self.home_dir).and_then(|s| s.channel_failure_end(sid, row));
        let ended_at = failed_end.as_deref().or_else(|| last.map(|m| m.timestamp.as_str()));
        let status = if failed_end.is_some() { "failed" } else if last.is_some() { "completed" } else { "no_reply" };
        self.bound_history_recheck(ctx, agent, Mode::Filtered)?;
        Ok(
            json!({"run":{"id":handles::issue_run(&self.home_dir,&ctx.user_id,run_id)?,"session_id":handles::issue(&self.home_dir,&ctx.user_id,sid)?,"agent_id":agent,"channel":channel,"started_at":first.timestamp,"ended_at":ended_at,"status":status},"events":events,"event_sources":{"text":"conversation history"},"not_persisted":["thinking_summaries"],"omitted":["tool_steps"],"read_only":true,"privacy_mode":if external {"masked"}else{"own"},"ner":transcript.ner}),
        )
    }
}
