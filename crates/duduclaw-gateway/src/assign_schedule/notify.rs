//! Telling the creator that the gateway paused one of their scheduled
//! assignments: a plain message to each of their verified channel
//! identities (DMs with the deployment's bots), best-effort. The dashboard
//! shows the paused state and reason either way.

use std::path::Path;

use crate::task_store::AssignScheduleRow;
use crate::trigger_route::texts::{Lang, lang_for};

/// The message for a pause with `reason`.
pub(crate) fn pause_text(lang: Lang, title: &str, reason: &str) -> String {
    let title = duduclaw_core::truncate_chars(title, 60);
    let why = match (lang, reason) {
        (Lang::ZhTw, "access_lost") => "你已經沒有交辦給這位 AI 員工的權限",
        (Lang::ZhTw, "agent_unavailable") => "這位 AI 員工目前停用或不存在",
        (Lang::ZhTw, "schedule_restricted") => "執行頻率不符合目前的排程限制",
        (Lang::ZhTw, _) => "連續幾次都沒能建立任務",
        (Lang::En, "access_lost") => {
            "you no longer have permission to assign work to this AI staff member"
        }
        (Lang::En, "agent_unavailable") => "this AI staff member is disabled or no longer exists",
        (Lang::En, "schedule_restricted") => {
            "the repeat interval no longer meets the current scheduling limit"
        }
        (Lang::En, _) => "the task could not be created several times in a row",
        (Lang::Ja, "access_lost") => "この AI スタッフにタスクを任せる権限がなくなりました",
        (Lang::Ja, "agent_unavailable") => "この AI スタッフは停止中か、存在しません",
        (Lang::Ja, "schedule_restricted") => "実行間隔が現在の予約制限に適合しません",
        (Lang::Ja, _) => "タスクを続けて作成できませんでした",
    };
    match lang {
        Lang::ZhTw => format!(
            "⏸ 排程交辦「{title}」這次沒有執行，已暫停：{why}。可在 dashboard 的「我交辦的」查看。"
        ),
        Lang::En => format!(
            "⏸ The scheduled assignment \"{title}\" did not run and is paused: {why}. See \"Assigned by me\" in the dashboard."
        ),
        Lang::Ja => format!(
            "⏸ 予約した依頼「{title}」は実行されず、一時停止しました：{why}。dashboard の「自分が任せた」で確認できます。"
        ),
    }
}

/// Send the pause notice to the creator's linked channels.
pub(super) async fn creator_paused(home: &Path, row: &AssignScheduleRow, reason: &str) {
    let text = pause_text(lang_for(home), &row.title, reason);
    let db = match crate::decision_notify::open_user_db(home) {
        Ok(Some(db)) => db,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(error = %e, "scheduled assignment: identity store unavailable for the pause notice");
            return;
        }
    };
    let channels = match db.verified_channels_for_user(&row.creator_user_id) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "scheduled assignment: channel lookup failed for the pause notice");
            return;
        }
    };
    if channels.is_empty() {
        tracing::info!(schedule = %row.id, "scheduled assignment paused; creator has no linked channel");
        return;
    }
    let http = reqwest::Client::new();
    for ident in channels {
        crate::install_notify::send_plain_text(
            home,
            &http,
            &ident.channel,
            &ident.channel_user_id,
            &text,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pause_texts_name_the_reason_in_each_language() {
        assert!(pause_text(Lang::ZhTw, "週報", "access_lost").contains("權限"));
        assert!(pause_text(Lang::En, "weekly", "agent_unavailable").contains("disabled"));
        assert!(pause_text(Lang::Ja, "週報", "create_failed").contains("作成"));
    }
}
