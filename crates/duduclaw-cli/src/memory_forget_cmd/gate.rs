//! The dashboard approval in front of `apply` (P2-B C-1).
//!
//! `plan` files one approval request bound to the plan id and hash
//! (`duduclaw_gateway::memory_forget_approval`); `apply --confirm` runs only
//! when an Admin approved exactly that plan in the dashboard. There is no
//! switch for this gate.

use std::path::Path;

use duduclaw_core::error::{DuDuClawError, Result};
use duduclaw_gateway::memory_forget_approval::{self as approval, GateVerdict};
use duduclaw_memory::ForgetPlan;

/// What the operator is told whenever apply is refused by the gate.
pub(crate) const APPROVE_IN_DASHBOARD: &str = "請到儀表板的待辦核准這筆忘記請求";

/// Counts for the approval card (no content, no ids).
pub(crate) fn card_counts(p: &ForgetPlan, review_pages: usize) -> serde_json::Value {
    let b = &p.document.body;
    let memories = b.targets.iter().filter(|t| t.store == "memories").count();
    serde_json::json!({
        "memories": memories,
        "key_facts": b.targets.len() - memories,
        "archive_copies": b.archive_ids.len(),
        "session_messages": b.session_messages.len(),
        "wiki_pages": b.wiki_pages.len(),
        "pages_needing_review": review_pages,
        "collateral_sources": b.collateral.len(),
        "tombstones": b.tombstones.len(),
    })
}

/// Fixed text on every forget card and notice (N3): the request came from a
/// local command line, and nothing proves who typed it.
pub(crate) const ORIGIN_NOTICE: &str = duduclaw_gateway::memory_forget_approval::ORIGIN_NOTICE;

/// The one-paragraph card summary: who, which conversation, how much.
pub(crate) fn card_summary(p: &ForgetPlan) -> String {
    let d = &p.document;
    let b = &d.body;
    let memories = b.targets.iter().filter(|t| t.store == "memories").count();
    let scope = if d.selector.messages.is_empty() {
        match d.selector.upto_seq {
            Some(n) => format!("整段對話（到訊息 #{n}）"),
            None => "整段對話".to_string(),
        }
    } else {
        let shown: Vec<&str> = d
            .selector
            .messages
            .iter()
            .take(8)
            .map(String::as_str)
            .collect();
        let more = d.selector.messages.len().saturating_sub(shown.len());
        let tail = if more > 0 {
            format!(" 等 {} 則", d.selector.messages.len())
        } else {
            String::new()
        };
        format!("訊息 {}{tail}", shown.join("、"))
    };
    format!(
        "依來源刪除記憶：命名空間 {agent}，來源：對話 {session}，{scope}。\
         將刪除記憶 {memories} 筆、關鍵事實 {facts} 筆，隱藏對話訊息 {msgs} 則，\
         刪除自動建檔頁 {pages} 頁；之後不再從這個來源學到任何東西。\
         計畫 {pid}，建立於 {created}。\n{ORIGIN_NOTICE}",
        created = d.created_at,
        agent = duduclaw_core::truncate_chars(&d.agent_id, 64),
        session = duduclaw_core::truncate_chars(&d.selector.session, 64),
        facts = b.targets.len() - memories,
        msgs = b.session_messages.len(),
        pages = b.wiki_pages.len(),
        pid = duduclaw_core::truncate_chars(&p.plan_id, 16),
    )
}

/// File the request for a new plan. Failure is an error: a plan nobody can
/// approve would only be refused at apply.
pub(crate) async fn file_request(
    home: &Path,
    p: &ForgetPlan,
    review_pages: usize,
) -> Result<String> {
    approval::request_for_plan(home, p, &card_summary(p), card_counts(p, review_pages))
        .await
        .map(|id| id.to_string())
        .map_err(|e| DuDuClawError::Memory(format!("無法送出核准請求：{e}")))
}

/// The gate decision for `apply --confirm`: `Ok(())` only when approved for
/// this exact plan hash. Otherwise `(audit reason, message)`.
pub(crate) async fn check(
    home: &Path,
    p: &ForgetPlan,
) -> std::result::Result<(), (String, String)> {
    let verdict = match approval::verdict_for_plan(home, p).await {
        Ok(v) => v,
        Err(e) => {
            return Err((
                "approval_unreadable".into(),
                format!("讀不到核准紀錄（{e}），不套用。{APPROVE_IN_DASHBOARD}。"),
            ));
        }
    };
    match verdict {
        GateVerdict::Approved { .. } => Ok(()),
        GateVerdict::Pending { approval_id } => Err((
            "approval_pending".into(),
            format!(
                "這筆忘記請求還沒有核准（編號 {}）。{APPROVE_IN_DASHBOARD}，核准後再執行一次。",
                duduclaw_core::truncate_chars(&approval_id, 8)
            ),
        )),
        GateVerdict::Refused { status, .. } => Err((
            format!("approval_{status}"),
            format!(
                "這筆忘記請求已{}，不能套用。請重新執行 plan，再{APPROVE_IN_DASHBOARD}。",
                match status.as_str() {
                    "denied" => "被拒絕或撤回",
                    "expired" => "逾期",
                    _ => "失效",
                }
            ),
        )),
        GateVerdict::HashMismatch { .. } => Err((
            "approval_hash_mismatch".into(),
            format!(
                "核准的內容和目前的計畫不同，不能套用。請重新執行 plan，再{APPROVE_IN_DASHBOARD}。"
            ),
        )),
        GateVerdict::Missing => {
            // A plan from before the gate existed, or whose request failed:
            // file one now so it can be approved.
            let filed = file_request(home, p, 0).await;
            Err((
                "approval_missing".into(),
                match filed {
                    Ok(id) => format!(
                        "這份計畫還沒有核准請求，已補送一筆（編號 {}）。{APPROVE_IN_DASHBOARD}，核准後再執行一次。",
                        duduclaw_core::truncate_chars(&id, 8)
                    ),
                    Err(e) => format!("{e}。{APPROVE_IN_DASHBOARD}。"),
                },
            ))
        }
    }
}

/// One line for `apply` without `--confirm`: where the approval stands.
pub(crate) async fn status_line(home: &Path, p: &ForgetPlan) -> String {
    match approval::verdict_for_plan(home, p).await {
        Ok(GateVerdict::Approved { .. }) => "核准狀態：已在儀表板核准。\n".into(),
        Ok(GateVerdict::Pending { .. }) => {
            format!("核准狀態：等待核准。{APPROVE_IN_DASHBOARD}。\n")
        }
        Ok(GateVerdict::Missing) => {
            "核准狀態：尚未送出核准請求（加上 --confirm 時會補送）。\n".into()
        }
        Ok(_) => "核准狀態：已失效，請重新執行 plan。\n".into(),
        Err(e) => format!("核准狀態：讀不到（{e}）。\n"),
    }
}
