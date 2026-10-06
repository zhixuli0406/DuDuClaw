//! Channel notices for a goal whose content is limited to a TaskPacket
//! audience that does not name the channel (F3, V-M-10).
//!
//! Verified before the fix: `goal_notify` put the task title, the judge's
//! rejection text, the result summary and an LLM-written trajectory of the
//! work into the source group or the employee's notify channel, whatever the
//! task's audience. A chat channel is not a dashboard identity, so the
//! audience cannot be checked there. For such a task the channel only learns
//! that something happened, with the task's short id and a dashboard link;
//! the decision itself is made on the dashboard, where the audience applies.
use std::path::Path;

use crate::goal_notify::GoalProgress;
use crate::task_store::TaskRow;

/// `true` when chat channel `channel` may carry this task's content and
/// decide it: the task has no human-facing audience, or the audience names
/// `channel:<channel>`. Role names in packets never limit a channel; an
/// unreadable packet set does (fail closed).
pub(crate) fn channel_may_carry(home: &Path, task_id: &str, channel: &str) -> bool {
    crate::review_evidence::audience::channel_may_read_task(
        channel,
        &crate::review_evidence::audience::task_audience(home, task_id),
    )
}

/// What the dashboard calls the place to look.
const SEE_DASHBOARD: &str = "內容只開放給指定對象，請到儀表板查看。";

/// Progress line without title, feedback or summary.
pub(crate) fn private_progress_body(task: &TaskRow, progress: &GoalProgress) -> String {
    let short = duduclaw_core::truncate_chars(&task.id, 8);
    let what = match progress {
        GoalProgress::Dispatched { iter, cap, retry } => {
            let verb = if *retry { "重試" } else { "開始執行" };
            format!("🐾 目標 #{short} {verb}（第 {iter}/{cap} 輪）。")
        }
        GoalProgress::TeamDispatched { minutes } => {
            format!("🐾 目標 #{short} 已交給團隊，預計 {minutes} 分鐘回報。")
        }
        GoalProgress::Reviewing => format!("🔍 目標 #{short} 已產出結果，驗收中。"),
        GoalProgress::Rejected { iter, cap } => {
            format!("↩️ 目標 #{short} 第 {iter}/{cap} 輪未通過，修正後重試。")
        }
        GoalProgress::Done => format!("✅ 目標 #{short} 已完成。"),
        GoalProgress::NeedsHuman => format!("🧭 目標 #{short} 需要你的決定。"),
        GoalProgress::Kickoff => format!("⏳ 目標 #{short} 需先核准才會開始。"),
        GoalProgress::NoProgressReport { minutes } => {
            format!("⏱️ 目標 #{short} 已執行 {minutes} 分鐘未回報進度。")
        }
    };
    format!("{what}\n{SEE_DASHBOARD}")
}

/// needs_human / observer notice without buttons or content.
pub(crate) fn private_decision_notice(task: &TaskRow, link: Option<&str>) -> String {
    let short = duduclaw_core::truncate_chars(&task.id, 8);
    let link = link.map(|l| format!("\n{l}")).unwrap_or_default();
    format!("🧭 目標 #{short} 需要人處理。{SEE_DASHBOARD}這件事只能在儀表板決定。{link}")
}

/// Refusal for a channel press on a limited task (an older card).
pub(crate) const PRIVATE_CHANNEL_DECISION_REFUSED: &str =
    "這件事的內容只開放給指定對象，請到儀表板決定。";

#[cfg(test)]
mod tests {
    use super::*;

    fn task() -> TaskRow {
        let mut t = TaskRow::new(
            "abcdef1234".into(),
            "機密報價".into(),
            "secret description".into(),
            "normal".into(),
            "sales".into(),
            "system".into(),
        );
        t.judge_feedback = Some("secret feedback".into());
        t.result_summary = Some("secret result".into());
        t
    }

    #[test]
    fn private_notices_carry_no_task_text() {
        let t = task();
        for p in [
            GoalProgress::Dispatched {
                iter: 1,
                cap: 5,
                retry: false,
            },
            GoalProgress::Rejected { iter: 1, cap: 5 },
            GoalProgress::Done,
            GoalProgress::NeedsHuman,
            GoalProgress::Kickoff,
            GoalProgress::NoProgressReport { minutes: 10 },
        ] {
            let body = private_progress_body(&t, &p);
            for secret in ["機密報價", "secret"] {
                assert!(!body.contains(secret), "{body}");
            }
            assert!(body.contains("#abcdef12"));
        }
        let notice = private_decision_notice(&t, Some("https://x/tasks/abcdef1234"));
        assert!(!notice.contains("secret") && !notice.contains("機密"));
    }

    fn write_packet(home: &Path, task: &str, body: Option<Vec<&str>>) {
        let dir = home
            .join(duduclaw_core::task_packet::TEAM_PACKETS_DIR)
            .join(task)
            .join("1");
        std::fs::create_dir_all(&dir).unwrap();
        let raw = match body {
            None => "{".to_string(),
            Some(aud) => serde_json::json!({
                "packet_id": "p", "goal_id": task, "round": 1,
                "from_role": "executor", "to_role": "verifier",
                "objective": "x", "output_format": "files",
                "audience": aud
            })
            .to_string(),
        };
        std::fs::write(dir.join("p.json"), raw).unwrap();
    }

    #[test]
    fn only_human_facing_entries_limit_a_channel() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        assert!(channel_may_carry(h, "t1", "telegram"), "no packets");
        write_packet(h, "t1", Some(vec!["verifier"]));
        assert!(
            channel_may_carry(h, "t1", "telegram"),
            "role names do not limit people"
        );
        assert!(channel_may_carry(h, "t1", "line"));
        write_packet(h, "t1", Some(vec!["user:alice"]));
        assert!(!channel_may_carry(h, "t1", "telegram"));
        write_packet(h, "t1", Some(vec!["channel:telegram", "verifier"]));
        assert!(channel_may_carry(h, "t1", "telegram"));
        assert!(!channel_may_carry(h, "t1", "line"));
        assert!(!channel_may_carry(h, "t1", "telegram-x"), "exact match");
        write_packet(h, "t1", None);
        assert!(
            !channel_may_carry(h, "t1", "telegram"),
            "corrupt packet fails closed"
        );
    }

    async fn press(
        home: &Path,
        packet: Option<Vec<&str>>,
        channel: &str,
    ) -> Result<String, String> {
        let store = crate::task_store::TaskStore::open(home).unwrap();
        let mut t = task();
        t.status = "needs_human".into();
        if store.get_task(&t.id).await.unwrap().is_none() {
            store.insert_task(&t).await.unwrap();
        }
        write_packet(home, &t.id, packet);
        crate::goal_notify::apply_needs_human(
            home,
            channel,
            "42",
            &t.id,
            crate::decision_action::DecisionAct::Done,
        )
        .await
    }

    #[tokio::test]
    async fn channel_press_follows_the_channel_rule() {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        // Refused by the privacy rule only where the channel is excluded.
        let refused = |r: &Result<String, String>| {
            r.as_ref().err().map(String::as_str) == Some(PRIVATE_CHANNEL_DECISION_REFUSED)
        };
        assert!(refused(&press(h, None, "telegram").await));
        assert!(refused(
            &press(h, Some(vec!["user:alice"]), "telegram").await
        ));
        assert!(refused(
            &press(h, Some(vec!["channel:telegram"]), "line").await
        ));
        // Same as with no packet: on to the ordinary press authorization
        // (which refuses this unknown chat user for its own reason).
        assert!(!refused(
            &press(h, Some(vec!["verifier"]), "telegram").await
        ));
        assert!(!refused(
            &press(h, Some(vec!["channel:telegram"]), "telegram").await
        ));
        let store = crate::task_store::TaskStore::open(h).unwrap();
        assert_eq!(
            store.get_task(&task().id).await.unwrap().unwrap().status,
            "needs_human"
        );
    }
}
