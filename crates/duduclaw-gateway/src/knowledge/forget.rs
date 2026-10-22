//! Forget by source for a knowledge document (coarse, P0).
//!
//! When a source is deleted with "also forget what the employees learned
//! from it", the turns that **cited** it (`turn_citations`) are grouped by
//! (employee, conversation) and each group becomes one forget-by-source plan
//! over those turns' messages (the user message and the reply). Each plan
//! files the existing dashboard-only, Admin-only forget approval
//! (`memory_forget_approval`); nothing is forgotten before an Admin
//! approves it and the plan is applied.
//!
//! This is deliberately coarse: everything those turns taught the employee
//! is forgotten, not only what came from the document. A precise
//! document-lineage source is later work.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use duduclaw_memory::{ForgetSelector, PlanOptions, PlanOutcome, SqliteMemoryEngine};
use serde::Serialize;

/// Most forget requests one deletion files; the rest are reported as
/// skipped (an Admin can plan them from the command line).
pub const MAX_FORGET_CARDS: usize = 20;

/// One filed plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FiledPlan {
    pub agent_id: String,
    pub plan_id: String,
    pub approval_id: String,
}

/// What [`plan_for_source`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ForgetSummary {
    /// Conversations that cited the source.
    pub conversations: usize,
    pub filed: Vec<FiledPlan>,
    /// Conversations with nothing left to forget.
    pub nothing_to_forget: usize,
    /// zh-TW reasons for groups that could not be planned.
    pub failed: Vec<String>,
    /// Conversations over [`MAX_FORGET_CARDS`], not planned.
    pub skipped: usize,
}

/// The cited turns of `source_id`, grouped by (employee, session), as
/// message keys.
pub fn groups_for_source(
    home: &Path,
    source_id: &str,
) -> duduclaw_knowledge::KbResult<BTreeMap<(String, String), Vec<String>>> {
    let conn = duduclaw_knowledge::schema::open_home(home)?;
    let rows = duduclaw_knowledge::citations::turns_citing_source(&conn, source_id)?;
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for r in rows {
        let keys = groups
            .entry((r.agent_id.clone(), r.session_id.clone()))
            .or_default();
        let mut push = |k: String| {
            if !keys.contains(&k) {
                keys.push(k);
            }
        };
        if let Some(k) = r.user_message_key.clone() {
            push(k);
        }
        if let Some(id) = r.assistant_message_id {
            push(format!("m:{id}"));
        }
    }
    groups.retain(|_, keys| !keys.is_empty());
    Ok(groups)
}

fn open_engine(home: &Path) -> Result<SqliteMemoryEngine, String> {
    let db: PathBuf = home.join("memory.db");
    if !db.is_file() {
        return Err("找不到 memory.db".into());
    }
    SqliteMemoryEngine::new(&db).map_err(|e| format!("無法開啟 memory.db：{e}"))
}

fn counts(p: &duduclaw_memory::ForgetPlan) -> serde_json::Value {
    let b = &p.document.body;
    let memories = b.targets.iter().filter(|t| t.store == "memories").count();
    serde_json::json!({
        "memories": memories,
        "key_facts": b.targets.len() - memories,
        "session_messages": b.session_messages.len(),
        "wiki_pages": b.wiki_pages.len(),
    })
}

/// Plan and file forget requests for every conversation that cited
/// `source_id`. `actor` is the dashboard user deleting the source.
pub async fn plan_for_source(
    home: &Path,
    source_id: &str,
    title: &str,
    actor: &str,
) -> ForgetSummary {
    let (h, sid) = (home.to_path_buf(), source_id.to_string());
    let groups = match super::blocking(move || groups_for_source(&h, &sid)).await {
        Ok(g) => g,
        Err(e) => {
            return ForgetSummary {
                failed: vec![format!("讀不到引用紀錄：{}", e.public_message())],
                ..Default::default()
            };
        }
    };
    let mut out = ForgetSummary {
        conversations: groups.len(),
        skipped: groups.len().saturating_sub(MAX_FORGET_CARDS),
        ..Default::default()
    };
    if groups.is_empty() {
        return out;
    }
    let engine = match open_engine(home) {
        Ok(e) => e,
        Err(e) => {
            out.failed.push(e);
            return out;
        }
    };
    for ((agent, session), keys) in groups.into_iter().take(MAX_FORGET_CARDS) {
        let selector = ForgetSelector {
            session: session.clone(),
            messages: keys,
            upto_seq: None,
            upto_time: Some(chrono::Utc::now()),
        };
        let opts = PlanOptions::default();
        let external = match crate::memory_forget_steps::collect_external_for_plan(
            &engine, home, &agent, &selector, opts,
        )
        .await
        {
            Ok(x) => x.unwrap_or_default(),
            Err(e) => {
                out.failed.push(format!("{agent}：{e}"));
                continue;
            }
        };
        match engine
            .plan_forget_source(&agent, &selector, opts, &external)
            .await
        {
            Ok(PlanOutcome::Planned(plan)) => {
                let summary = format!(
                    "刪除知識庫文件「{}」時一併忘記：{agent} 在 {} 則訊息中引用過它（操作者 {actor}）",
                    duduclaw_core::truncate_chars(title, 60),
                    selector.messages.len(),
                );
                match crate::memory_forget_approval::request_for_plan_from(
                    home,
                    &plan,
                    &summary,
                    counts(&plan),
                    Some(crate::memory_forget_approval::ORIGIN_KNOWLEDGE_DELETE),
                )
                .await
                {
                    Ok(id) => out.filed.push(FiledPlan {
                        agent_id: agent,
                        plan_id: plan.plan_id.clone(),
                        approval_id: id.to_string(),
                    }),
                    Err(e) => out.failed.push(format!("{agent}：{e}")),
                }
            }
            Ok(PlanOutcome::NothingToForget { .. }) => out.nothing_to_forget += 1,
            Ok(PlanOutcome::TooLarge { reason }) => out
                .failed
                .push(format!("{agent}：範圍太大（{reason}），沒有建立計畫")),
            Err(e) => out.failed.push(format!("{agent}：{e}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use duduclaw_knowledge::citations::{TurnCitation, record_turn};

    fn row(
        turn: &str,
        agent: &str,
        session: &str,
        user: i64,
        reply: i64,
        source: &str,
    ) -> TurnCitation {
        TurnCitation {
            turn_id: turn.into(),
            agent_id: agent.into(),
            session_id: session.into(),
            user_message_key: Some(format!("m:{user}")),
            assistant_message_id: Some(reply),
            n: 1,
            chunk_id: Some(1),
            source_id: source.into(),
            title: "員工手冊.pdf".into(),
            locator: "第 3 頁".into(),
            excerpt: String::new(),
            cited: true,
            at: duduclaw_knowledge::now_rfc3339(),
        }
    }

    #[test]
    fn groups_cited_turns_by_employee_and_conversation() {
        let home = tempfile::tempdir().unwrap();
        let mut conn = duduclaw_knowledge::schema::open_home(home.path()).unwrap();
        record_turn(
            &mut conn,
            &[
                row("t1", "hr-bot", "line:U1", 10, 11, "src_a"),
                row("t2", "hr-bot", "line:U1", 12, 13, "src_a"),
                row("t3", "hr-bot", "webchat:x", 5, 6, "src_a"),
                row("t4", "sales-bot", "line:U1", 20, 21, "src_b"),
            ],
        )
        .unwrap();
        // An offered-but-not-cited row does not count.
        let mut uncited = row("t5", "hr-bot", "line:U9", 30, 31, "src_a");
        uncited.cited = false;
        record_turn(&mut conn, &[uncited]).unwrap();
        let g = groups_for_source(home.path(), "src_a").unwrap();
        assert_eq!(g.len(), 2);
        assert_eq!(
            g[&("hr-bot".to_string(), "line:U1".to_string())],
            vec!["m:10", "m:11", "m:12", "m:13"]
        );
        assert!(
            groups_for_source(home.path(), "src_none")
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn no_citations_files_nothing_and_a_missing_memory_db_is_reported() {
        let home = tempfile::tempdir().unwrap();
        let s = plan_for_source(home.path(), "src_x", "t", "admin").await;
        assert_eq!(s, ForgetSummary::default());
        let mut conn = duduclaw_knowledge::schema::open_home(home.path()).unwrap();
        record_turn(&mut conn, &[row("t1", "hr-bot", "line:U1", 1, 2, "src_x")]).unwrap();
        let s = plan_for_source(home.path(), "src_x", "t", "admin").await;
        assert_eq!(s.conversations, 1);
        assert!(s.filed.is_empty());
        assert_eq!(s.failed.len(), 1);
    }
}
