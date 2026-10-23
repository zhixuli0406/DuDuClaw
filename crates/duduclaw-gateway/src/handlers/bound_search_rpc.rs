use super::*;
use crate::history_privacy::{Mode, handles, projection};
impl MethodHandler {
    pub(super) async fn bound_conversation_search(
        &self,
        ctx: &UserContext,
        agent: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<crate::search_index::SearchHit>, String> {
        crate::team_history::limits::take(&self.home_dir, &format!("bound:{}", ctx.user_id), false)
            .map_err(|_| "history rate limited")?;
        let home = self.home_dir.clone();
        let user = ctx.user_id.clone();
        let a = agent.to_owned();
        let permit = crate::history_privacy::read::permit()?;
        let sessions =
            tokio::task::spawn_blocking(move || projection::load(&home, &user, &a, permit))
                .await
                .map_err(|_| "history unavailable")??;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);
        let needle = query.to_lowercase();
        let mut hits = vec![];
        for s in sessions {
            let external = projection::external(&s.metadata.class);
            let masked = tokio::time::timeout_at(
                deadline,
                Box::pin(projection::project(
                    self.home_dir.clone(),
                    projection::clone_rows(&s.rows),
                    external,
                    agent.into(),
                )),
            )
            .await
            .map_err(|_| "masking timeout")??;
            if masked.messages.len() != s.rows.len() {
                return Err("history unavailable".into());
            }
            for (m, raw) in masked.messages.iter().zip(s.rows.iter()) {
                let content = crate::history_privacy::plain_parts(&m.parts);
                if !content.to_lowercase().contains(&needle) {
                    continue;
                }
                let id = if external {
                    handles::issue(&self.home_dir, &ctx.user_id, &s.metadata.id)?
                } else {
                    s.metadata.id.clone()
                };
                hits.push(crate::search_index::SearchHit {source:crate::search_index::SOURCE_CONVERSATION,id:handles::reference(&self.home_dir,&ctx.user_id,&format!("{}:{}",s.metadata.id,raw.id))?,title:duduclaw_core::truncate_chars(&content,80),snippet:duduclaw_core::truncate_chars(&content,400),agent_id:Some(agent.into()),timestamp:Some(m.timestamp.clone()),jump:json!({"session_id":id,"role":m.role,"read_only":external,"channel":projection::channel(&s.metadata.class)})});
            }
        }
        hits.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        hits.truncate(limit);
        self.bound_history_recheck(ctx, agent, Mode::Filtered)?;
        Ok(hits)
    }
}
