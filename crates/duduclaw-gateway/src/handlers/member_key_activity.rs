//! Activity summaries can contain conversation excerpts. A task/agent ACL alone
//! does not prove ownership of the conversation from which that text came.
use super::*;

impl MethodHandler {
    pub(super) async fn member_key_activity_rows(
        &self, ctx: &UserContext, rows: Vec<ActivityRow>,
    ) -> Result<Vec<ActivityRow>, WsFrame> {
        if ctx.member_key_id.is_none() { return Ok(rows); }
        let refused = || WsFrame::error_response("", "目前無法確認這把金鑰的使用範圍，請稍後再試。");
        let db = self.user_db.read().await.clone().ok_or_else(refused)?;
        let home = self.home_dir.clone();
        let ctx = ctx.clone();
        tokio::task::spawn_blocking(move || -> Result<Vec<ActivityRow>, WsFrame> {
            let live = crate::member_keys::refresh_authority(&home, &db, &ctx).map_err(|_| refused())?;
            // A missing source store proves ownership of no rows; redact all text.
            let source = crate::history_privacy::read::open(&home).ok();
            Ok(rows.into_iter().map(|mut row| {
                let own = live.include_conversations && source.as_ref().is_some_and(|source| {
                    let metadata = row.metadata.as_deref().and_then(|raw| serde_json::from_str::<Value>(raw).ok());
                    let session = metadata.as_ref().and_then(|v| v.get("session_id")).and_then(Value::as_str);
                    session.is_some_and(|id| {
                        matches!(crate::history_privacy::read::metadata(source, &live.context.user_id, id),
                            Ok(Some(meta)) if meta.agent == row.agent_id
                                && crate::history_privacy::key_class_allowed(&live.context, &meta.class))
                    })
                });
                if !own {
                    // Legacy conversation events have no provenance. Keep only
                    // structural feed fields; never infer an owner from the agent.
                    row.summary.clear();
                    row.metadata = None;
                }
                row
            }).collect())
        }).await.map_err(|_| refused())?
    }
}
