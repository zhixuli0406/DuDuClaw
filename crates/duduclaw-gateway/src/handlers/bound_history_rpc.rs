//! Direct bindings are independent of manager oversight. Restricted history
//! never changes the WebChat send/call lane or grants task content access.
use super::*;
use crate::history_privacy::{self as privacy, Class, Mode, handles, read};
use crate::team_history::{audit, limits, mask};
use std::collections::BTreeMap;

impl MethodHandler {
    pub(super) fn bound_history_live(&self, ctx: &UserContext) -> Result<UserContext, String> {
        let live = task_privacy::live_reader_context(&self.home_dir, ctx)
            .map_err(|_| "session not found")?;
        if live.requires_password_change() {
            return Err("session not found".into());
        }
        Ok(live)
    }

    pub(super) fn bound_history_recheck(
        &self,
        ctx: &UserContext,
        agent: &str,
        initial: Mode,
    ) -> Result<(), String> {
        let live = self.bound_history_live(ctx)?;
        let current =
            privacy::mode(&self.home_dir, &live, agent).map_err(|_| "session not found")?;
        if initial == Mode::Full && current != Mode::Full {
            return Err("history authority changed; refresh".into());
        }
        Ok(())
    }

    pub(super) async fn bound_history_list(
        &self,
        ctx: &UserContext,
        agent: &str,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        limits::take(&self.home_dir, &format!("bound:{}", ctx.user_id), false)
            .map_err(|_| "history rate limited")?;
        let home = self.home_dir.clone();
        let user = ctx.user_id.clone();
        let agent_owned = agent.to_owned();
        let permit = read::permit()?;
        let list = tokio::task::spawn_blocking(move || {
            let _permit=permit;
            let c = read::open(&home)?;
            let entries = read::list(&c, &user, &agent_owned, limit.max(1))?;
            let mut out = Vec::new();
            for m in entries {
                let external = matches!(m.class, Class::External {..});
                let channel = match &m.class { Class::External {channel} => channel.as_str(), _ => "webchat" };
                // Stored external titles and summaries may be derived from
                // private or pre-filter content. A generic label needs no NER.
                let title = if external { Some("External conversation".to_owned()) } else { m.title };
                let id = if external { handles::issue(&home, &user, &m.id)? } else { m.id.clone() };
                let turns: i64 = c.query_row("SELECT COUNT(*) FROM session_messages WHERE session_id=?1 AND hidden=0 AND undone_at IS NULL AND role IN ('user','assistant')",[&m.id],|r|r.get(0)).map_err(|_|"history unavailable")?;
                out.push(json!({"session_id":id,"agent_id":m.agent,"title":title,"last_active":m.last_active,"turns":turns,"tokens":0,"lineage":null,"channel":channel,"read_only":external,"title_masked":external}));
            }
            Ok::<_,String>(out)
        }).await.map_err(|_|"history unavailable")??;
        self.bound_history_recheck(ctx, agent, Mode::Filtered)?;
        if limit == 0 { Ok(vec![]) } else { Ok(list) }
    }

    pub(super) async fn bound_external_history(
        &self,
        ctx: &UserContext,
        id: &str,
        agent: &str,
        channel: &str,
        limit: usize,
    ) -> Result<Value, String> {
        limits::take(&self.home_dir, &format!("bound:{}", ctx.user_id), true)
            .map_err(|_| "history rate limited")?;
        let home = self.home_dir.clone();
        let raw = id.to_owned();
        let permit = read::permit()?;
        let rows = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let c = read::open(&home)?;
            read::rows(&c, &raw)
        })
        .await
        .map_err(|_| "history unavailable")??;
        let messages = rows.into_iter().map(|r| r.message).collect();
        let mut transcript = mask::external(
            self.home_dir.clone(),
            messages,
            BTreeMap::new(),
            agent.into(),
        )
        .await?;
        let start = transcript.messages.len().saturating_sub(limit);
        transcript.messages.drain(..start);
        mask::recount(&mut transcript);
        let messages: Vec<Value> = transcript.messages.iter().map(|m| {
            let content = privacy::plain_parts(&m.parts);
            json!({"role":m.role,"content":content,"sender_label":m.sender_label,"parts":m.parts,"timestamp":m.timestamp,"tokens":0})
        }).collect();
        let handle = handles::issue(&self.home_dir, &ctx.user_id, id)?;
        self.bound_history_recheck(ctx, agent, Mode::Filtered)?;
        let digest = audit::session_digest(&self.home_dir, id)?;
        audit::write(
            &self.home_dir,
            "bound_history_viewed",
            &ctx.user_id,
            agent,
            json!({"viewer":ctx.user_id,"session":digest,"view_mode":"masked","ner":transcript.ner}),
            None,
        )?;
        self.bound_history_recheck(ctx, agent, Mode::Filtered)?;
        Ok(
            json!({"session_id":handle,"agent_id":agent,"messages":messages,"read_only":true,"privacy_mode":"masked","channel":channel,"ner":transcript.ner}),
        )
    }
}
