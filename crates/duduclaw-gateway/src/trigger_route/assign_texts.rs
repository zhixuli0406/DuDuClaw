//! Words the bot says around `/交辦` (assigning a task from a chat), in the
//! home's configured language (see [`super::texts`]).

use super::texts::Lang;

/// The sender has no dashboard account we can name.
pub fn needs_identity(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "交辦任務需要先在 dashboard 綁定你的通訊帳號（個人設定 → 通道身分）。",
        Lang::En => {
            "To assign tasks, first link this messaging account to your dashboard account (Profile → Channel identities)."
        }
        Lang::Ja => {
            "タスクを任せるには、先に dashboard でこのメッセージアカウントを連携してください（個人設定 → チャネル ID）。"
        }
    }
}

/// The sender may talk to the employee but not hand them work.
pub fn call_only(lang: Lang, name: &str) -> String {
    match lang {
        Lang::ZhTw => format!("你可以和「{name}」對話，但沒有交辦任務的權限。"),
        Lang::En => format!("You can talk to {name}, but you don't have permission to assign them tasks."),
        Lang::Ja => format!("「{name}」と会話はできますが、タスクを任せる権限はありません。"),
    }
}

/// A channel that cannot tell who sent the command.
pub fn not_supported(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "這個通道還不支援交辦。",
        Lang::En => "Assigning tasks is not supported on this channel yet.",
        Lang::Ja => "このチャネルではまだタスクを任せられません。",
    }
}

/// After `/交辦 @name` with no task.
pub fn ask_for_task(lang: Lang, name: &str, minutes: u64) -> String {
    match lang {
        Lang::ZhTw => format!(
            "請說要交辦給「{name}」的事，你的下一則訊息會成為交辦內容（{minutes} 分鐘內有效）。"
        ),
        Lang::En => format!(
            "What task should {name} take on? Your next message becomes the task (valid for {minutes} minutes)."
        ),
        Lang::Ja => format!(
            "「{name}」に任せる内容を送ってください。次のメッセージがタスクになります（{minutes} 分間有効）。"
        ),
    }
}

/// How to use the command.
pub fn usage(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => {
            "📌 交辦任務\n\
             /交辦 @AI員工 要做的事 — 交辦一件任務，AI 員工完成或卡住時會回到這個對話通知你\n\
             可以用「||」再加：驗收標準、時限:24h、邊界:不可做的事、想一想（先給計畫再開工）\n\
             /交辦 — 看你可以交辦的 AI 員工\n\
             /交辦 狀態 — 看你交辦、還沒結束的任務\n\
             排程：在後面加「|| 時間:明天 15:00」或「|| 排程:每週一 9:00」；/交辦 排程 — 看、暫停或刪除你的排程"
        }
        Lang::En => {
            "📌 Assign a task\n\
             /assign @staff what to do — hand over a task; you are told here when it is done or stuck\n\
             Add with \"||\": acceptance criteria, duration:24h, risk:what not to do, plan (a plan first)\n\
             /assign — staff you can assign to\n\
             /assign status — your unfinished assigned tasks\n\
             Schedule it: add \"|| at:tomorrow 15:00\" or \"|| schedule:every monday 9:00\"; /assign schedules — list, pause or remove yours"
        }
        Lang::Ja => {
            "📌 タスクを任せる\n\
             /交辦 @スタッフ 内容 — タスクを任せます。完了や行き詰まりはこの会話に届きます\n\
             「||」で追加：受け入れ基準、時限:24h、邊界:してはいけないこと、想一想（先に計画）\n\
             /交辦 — 任せられるスタッフ\n\
             /交辦 狀態 — 任せた未完了のタスク\n\
             予約：後ろに「|| 時間:明天 15:00」や「|| 排程:每週一 9:00」；/交辦 排程 — 予約の確認・一時停止・削除"
        }
    }
}

/// Heading of the plain-text assignable list; `example` is one `@name`.
pub fn list_heading(lang: Lang, example: &str) -> String {
    match lang {
        Lang::ZhTw => format!("你可以交辦任務的 AI 員工（輸入「/交辦 {example} 要做的事」）："),
        Lang::En => format!("AI staff you can assign tasks to (type \"/assign {example} what to do\"):"),
        Lang::Ja => format!("タスクを任せられる AI スタッフ（「/交辦 {example} 内容」と入力）："),
    }
}

/// Above the quick-reply buttons of the assignable list.
pub fn list_intro(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "要交辦給哪位 AI 員工？點一下名字，再說要做的事。",
        Lang::En => "Who should take the task? Tap a name, then say what to do.",
        Lang::Ja => "どの AI スタッフに任せますか？名前をタップしてから内容を送ってください。",
    }
}

/// Nobody can be assigned to.
pub fn nobody_assignable(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "目前沒有你可以交辦任務的 AI 員工。",
        Lang::En => "There is no AI staff member you can assign tasks to right now.",
        Lang::Ja => "現在タスクを任せられる AI スタッフはいません。",
    }
}

/// The task could not be created (or the list read). Fixed text: the
/// cause goes to the log only, never into the chat.
pub fn failed(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "⚠️ 交辦沒有成功，請稍後再試；一直失敗的話請聯絡管理員。",
        Lang::En => "⚠️ The task was not assigned. Please try again later, or ask an administrator if it keeps failing.",
        Lang::Ja => "⚠️ タスクを任せられませんでした。しばらくしてから再度お試しください。続く場合は管理者に連絡してください。",
    }
}

/// The sender assigned too many tasks from chat (rate or open-task cap).
pub fn too_many(lang: Lang, per_window: u32, window_minutes: u64, max_open: u32) -> String {
    match lang {
        Lang::ZhTw => format!(
            "⚠️ 交辦太多了：從聊天交辦每 {window_minutes} 分鐘最多 {per_window} 件，還沒結束的最多 {max_open} 件。請稍後再試，或等手上的任務結束。"
        ),
        Lang::En => format!(
            "⚠️ Too many tasks: from a chat you can assign at most {per_window} every {window_minutes} minutes, and keep at most {max_open} unfinished. Try again later, or wait for some to finish."
        ),
        Lang::Ja => format!(
            "⚠️ 依頼が多すぎます。チャットからは {window_minutes} 分ごとに最大 {per_window} 件、未完了は最大 {max_open} 件までです。しばらくしてから、または進行中のタスクが終わってから再度お試しください。"
        ),
    }
}

/// What the confirmation says about the task.
pub struct Confirmation<'a> {
    pub agent_name: &'a str,
    pub short_id: &'a str,
    pub description: &'a str,
    /// `Some(owner)` when the right came from the reporting line (a report's
    /// employee): `owner` is that report's display name.
    pub team_owner: Option<&'a str>,
    /// Progress comes back to this conversation (else: the dashboard).
    pub progress_here: bool,
    /// The task waits for a plan to be approved first.
    pub plan_first: bool,
    /// A `邊界:` segment was dropped (only an Admin may set it).
    pub boundary_ignored: bool,
}

/// The reply after a task was created.
pub fn confirmation(lang: Lang, c: &Confirmation<'_>) -> String {
    let mut out = match lang {
        Lang::ZhTw => format!(
            "📌 已交辦給【{}】，任務 #{}\n內容：{}",
            c.agent_name, c.short_id, c.description
        ),
        Lang::En => format!(
            "📌 Assigned to {} — task #{}\nTask: {}",
            c.agent_name, c.short_id, c.description
        ),
        Lang::Ja => format!(
            "📌 【{}】に任せました。タスク #{}\n内容：{}",
            c.agent_name, c.short_id, c.description
        ),
    };
    if let Some(owner) = c.team_owner {
        out.push('\n');
        out.push_str(&match lang {
            Lang::ZhTw => format!("（依組織關係交辦：{owner}的 AI 員工）"),
            Lang::En => format!("(Assigned through your team: {owner}'s AI staff)"),
            Lang::Ja => format!("（組織関係で依頼：{owner}の AI スタッフ）"),
        });
    }
    if c.boundary_ignored {
        out.push('\n');
        out.push_str(match lang {
            Lang::ZhTw => "邊界設定只有管理員能改，已沿用預設。",
            Lang::En => "Only an administrator can set the risk boundary; the default applies.",
            Lang::Ja => "リスク境界は管理者のみ変更できます。既定の境界を使います。",
        });
    }
    if c.plan_first {
        out.push('\n');
        out.push_str(match lang {
            Lang::ZhTw => "會先產生執行計畫，等你在 dashboard 核准後才開始。",
            Lang::En => "A plan comes first; work starts after you approve it in the dashboard.",
            Lang::Ja => "まず実行計画を作成し、dashboard で承認されてから開始します。",
        });
    }
    out.push('\n');
    out.push_str(match (lang, c.progress_here) {
        (Lang::ZhTw, true) => {
            "進度和結果會回到這個對話。輸入「/交辦 狀態」查看你交辦的任務。"
        }
        (Lang::ZhTw, false) => {
            "進度和結果可以在「目標」頁的「我交辦的」查看。輸入「/交辦 狀態」查看你交辦的任務。"
        }
        (Lang::En, true) => {
            "Progress and the result come back to this conversation. Type \"/assign status\" to see your tasks."
        }
        (Lang::En, false) => {
            "See progress and the result under \"Assigned by me\" on the Goals page. Type \"/assign status\" to see your tasks."
        }
        (Lang::Ja, true) => {
            "進捗と結果はこの会話に届きます。「/交辦 狀態」で任せたタスクを確認できます。"
        }
        (Lang::Ja, false) => {
            "進捗と結果は「目標」ページの「自分が任せた」で確認できます。「/交辦 狀態」で任せたタスクを確認できます。"
        }
    });
    out
}

/// No unfinished assigned tasks.
pub fn status_empty(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "你目前沒有交辦中、還沒結束的任務。",
        Lang::En => "You have no unfinished assigned tasks.",
        Lang::Ja => "任せた未完了のタスクはありません。",
    }
}

/// Heading of the unfinished assigned tasks.
pub fn status_heading(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "📌 你交辦、還沒結束的任務：",
        Lang::En => "📌 Your unfinished assigned tasks:",
        Lang::Ja => "📌 任せた未完了のタスク：",
    }
}

/// A title this conversation may not show.
pub fn title_hidden(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "（內容只開放給指定對象，請到 dashboard 查看）",
        Lang::En => "(restricted — see the dashboard)",
        Lang::Ja => "（内容は指定された人だけが見られます。dashboard で確認してください）",
    }
}

/// A user-facing label for a task status (internal tokens never reach a chat).
pub fn status_label(lang: Lang, status: &str) -> &'static str {
    match lang {
        Lang::ZhTw => match status {
            "todo" => "待派工",
            "pending" => "待重試",
            "in_progress" => "執行中",
            "review" | "revising" => "驗收中",
            "needs_human" => "待你決定",
            "blocked" => "受阻",
            "done" => "已完成",
            "cancelled" => "已放棄",
            "failed" => "已失敗",
            _ => "進行中",
        },
        Lang::En => match status {
            "todo" => "queued",
            "pending" => "retrying",
            "in_progress" => "working",
            "review" | "revising" => "in review",
            "needs_human" => "needs you",
            "blocked" => "blocked",
            "done" => "done",
            "cancelled" => "cancelled",
            "failed" => "failed",
            _ => "in progress",
        },
        Lang::Ja => match status {
            "todo" => "待機中",
            "pending" => "再試行待ち",
            "in_progress" => "実行中",
            "review" | "revising" => "検収中",
            "needs_human" => "判断待ち",
            "blocked" => "停止中",
            "done" => "完了",
            "cancelled" => "中止",
            "failed" => "失敗",
            _ => "進行中",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmation_names_the_team_owner_only_for_team_grants() {
        let mut c = Confirmation {
            agent_name: "業務專員",
            short_id: "1a2b3c4d",
            description: "整理報價單",
            team_owner: None,
            progress_here: true,
            plan_first: false,
            boundary_ignored: false,
        };
        let plain = confirmation(Lang::ZhTw, &c);
        assert!(!plain.contains("邊界設定"));
        assert!(plain.contains("【業務專員】") && plain.contains("#1a2b3c4d"));
        assert!(!plain.contains("依組織關係"));
        c.team_owner = Some("王小明");
        assert!(confirmation(Lang::ZhTw, &c).contains("依組織關係交辦：王小明的 AI 員工"));
        c.progress_here = false;
        assert!(confirmation(Lang::En, &c).contains("Goals page"));
        c.boundary_ignored = true;
        assert!(confirmation(Lang::ZhTw, &c).contains("邊界設定只有管理員能改，已沿用預設"));
    }
}
