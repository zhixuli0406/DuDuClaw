//! Words the bot says around scheduled assignments (`/交辦 … || 時間:…`,
//! `/交辦 排程`), in the home's configured language.

use chrono::{DateTime, Utc};

use super::texts::Lang;
use crate::assign_schedule::{ScheduleSpec, SpecError, Zone};

const ZH_DAYS: [&str; 7] = ["一", "二", "三", "四", "五", "六", "日"];
const EN_DAYS: [&str; 7] = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const JA_DAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];

/// `10/26 09:00` in `zone`.
pub fn local(t: DateTime<Utc>, zone: Zone) -> String {
    zone.local_now(t).format("%m/%d %H:%M").to_string()
}

fn day_names(days: &[u8], names: &[&str; 7]) -> Vec<String> {
    days.iter()
        .filter_map(|d| usize::from(*d).checked_sub(1).and_then(|i| names.get(i)))
        .map(|s| s.to_string())
        .collect()
}

/// How often, in words.
pub fn describe(lang: Lang, spec: &ScheduleSpec, zone: Zone) -> String {
    let hm = |h: &u8, m: &u8| format!("{h:02}:{m:02}");
    match (lang, spec) {
        (_, ScheduleSpec::Cron { expr }) => format!("cron {expr}"),
        (_, ScheduleSpec::Once { at }) => {
            let when = crate::assign_schedule::spec::parse_utc(at)
                .map(|t| local(t, zone))
                .unwrap_or_default();
            match lang {
                Lang::ZhTw => format!("{when}（一次）"),
                Lang::En => format!("once at {when}"),
                Lang::Ja => format!("{when}（1回）"),
            }
        }
        (Lang::ZhTw, ScheduleSpec::Hourly { minute }) => format!("每小時 {minute:02} 分"),
        (Lang::En, ScheduleSpec::Hourly { minute }) => format!("every hour at :{minute:02}"),
        (Lang::Ja, ScheduleSpec::Hourly { minute }) => format!("毎時 {minute:02} 分"),
        (Lang::ZhTw, ScheduleSpec::Daily { hour, minute }) => format!("每天 {}", hm(hour, minute)),
        (Lang::En, ScheduleSpec::Daily { hour, minute }) => {
            format!("every day at {}", hm(hour, minute))
        }
        (Lang::Ja, ScheduleSpec::Daily { hour, minute }) => format!("毎日 {}", hm(hour, minute)),
        (Lang::ZhTw, ScheduleSpec::Weekdays { hour, minute }) => {
            format!("平日 {}", hm(hour, minute))
        }
        (Lang::En, ScheduleSpec::Weekdays { hour, minute }) => {
            format!("weekdays at {}", hm(hour, minute))
        }
        (Lang::Ja, ScheduleSpec::Weekdays { hour, minute }) => format!("平日 {}", hm(hour, minute)),
        (Lang::ZhTw, ScheduleSpec::Weekly { days, hour, minute }) => {
            format!(
                "每週{} {}",
                day_names(days, &ZH_DAYS).join("、"),
                hm(hour, minute)
            )
        }
        (Lang::En, ScheduleSpec::Weekly { days, hour, minute }) => {
            format!(
                "every {} at {}",
                day_names(days, &EN_DAYS).join(", "),
                hm(hour, minute)
            )
        }
        (Lang::Ja, ScheduleSpec::Weekly { days, hour, minute }) => {
            format!(
                "毎週 {} {}",
                day_names(days, &JA_DAYS).join("・"),
                hm(hour, minute)
            )
        }
    }
}

pub struct Created<'a> {
    pub agent_name: &'a str,
    pub when: &'a str,
    pub next: &'a str,
    pub short_id: &'a str,
    pub description: &'a str,
    pub boundary_ignored: bool,
}

pub fn created(lang: Lang, c: &Created<'_>) -> String {
    let Created {
        agent_name,
        when,
        next,
        short_id,
        description,
        boundary_ignored,
    } = c;
    let mut out = match lang {
        Lang::ZhTw => format!(
            "🗓 已排程（#{short_id}）：{when} 交辦給「{agent_name}」\n{description}\n下次：{next}。每次執行前都會重新確認你的權限。用「/交辦 排程」查看、暫停或刪除。"
        ),
        Lang::En => format!(
            "🗓 Scheduled (#{short_id}): {when}, assigned to {agent_name}\n{description}\nNext: {next}. Your permission is checked again before every run. Use \"/assign schedules\" to list, pause or remove."
        ),
        Lang::Ja => format!(
            "🗓 予約しました（#{short_id}）：{when}「{agent_name}」に依頼\n{description}\n次回：{next}。毎回の実行前に権限を確認し直します。「/交辦 排程」で確認・一時停止・削除できます。"
        ),
    };
    if *boundary_ignored {
        out.push('\n');
        out.push_str(match lang {
            Lang::ZhTw => "（邊界只有管理員能設定，這次沒有套用。）",
            Lang::En => "(Only an admin can set a risk boundary; it was not applied.)",
            Lang::Ja => "（境界は管理者のみ設定できるため、適用していません。）",
        });
    }
    out
}

pub fn spec_error(lang: Lang, e: &SpecError) -> String {
    match (lang, e) {
        (Lang::ZhTw, SpecError::CronAdminOnly) => "cron 排程只有管理員能設定。".into(),
        (Lang::ZhTw, SpecError::InThePast) => "這個時間已經過了（至少要一分鐘後）。".into(),
        (Lang::ZhTw, SpecError::TooFar) => "一次性的排程最多只能排到一年內。".into(),
        (Lang::ZhTw, SpecError::TooFrequent { min_minutes }) => {
            format!("排程太頻繁了，最短間隔是 {min_minutes} 分鐘。")
        }
        (Lang::En, SpecError::CronAdminOnly) => "Only an admin can use a cron schedule.".into(),
        (Lang::En, SpecError::InThePast) => {
            "That time has passed (it must be at least a minute ahead).".into()
        }
        (Lang::En, SpecError::TooFar) => "A one-time schedule can be at most a year ahead.".into(),
        (Lang::En, SpecError::TooFrequent { min_minutes }) => {
            format!("That runs too often; the shortest interval is {min_minutes} minutes.")
        }
        (Lang::Ja, SpecError::CronAdminOnly) => "cron の予約は管理者のみ設定できます。".into(),
        (Lang::Ja, SpecError::InThePast) => {
            "その時刻は過ぎています（1分以上先にしてください）。".into()
        }
        (Lang::Ja, SpecError::TooFar) => "1回だけの予約は1年以内にしてください。".into(),
        (Lang::Ja, SpecError::TooFrequent { min_minutes }) => {
            format!("頻度が高すぎます。最短間隔は {min_minutes} 分です。")
        }
        _ => when_usage(lang).to_string(),
    }
}

/// The time forms a `時間:` segment accepts.
pub fn when_usage(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => {
            "看不懂這個時間。可以這樣寫：\n\
             時間:明天 15:00、時間:10/25 下午2點、時間:2026-10-25 14:00\n\
             排程:每天 9:00、排程:平日 9:00、排程:每週一 9:00、排程:每週一、三 早上9點、排程:每小時"
        }
        Lang::En => {
            "I couldn't read that time. Try:\n\
             at:tomorrow 15:00, at:2026-10-25 14:00\n\
             schedule:daily 09:00, schedule:weekdays 09:00, schedule:every monday 9:00, schedule:hourly"
        }
        Lang::Ja => {
            "時刻を読み取れませんでした。例：\n\
             時間:明天 15:00、時間:2026-10-25 14:00\n\
             排程:每天 9:00、排程:平日 9:00、排程:每週一 9:00、排程:每小時"
        }
    }
}

pub fn too_many(lang: Lang, max: i64) -> String {
    match lang {
        Lang::ZhTw => format!("你已經有 {max} 個排程交辦（含暫停中的），請先刪掉用不到的再新增。"),
        Lang::En => format!(
            "You already have {max} scheduled assignments (paused ones included). Remove one first."
        ),
        Lang::Ja => format!(
            "予約した依頼がすでに {max} 件あります（一時停止中を含む）。不要なものを削除してください。"
        ),
    }
}

pub fn list_heading(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "🗓 你的排程交辦：",
        Lang::En => "🗓 Your scheduled assignments:",
        Lang::Ja => "🗓 予約した依頼：",
    }
}

pub fn list_empty(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "你還沒有排程交辦。在交辦後面加「|| 時間:每週一 9:00」就能排程。",
        Lang::En => {
            "You have no scheduled assignments. Add \"|| schedule:every monday 9:00\" to an assignment to schedule it."
        }
        Lang::Ja => {
            "予約した依頼はありません。依頼の後ろに「|| 時間:每週一 9:00」を付けると予約できます。"
        }
    }
}

pub fn list_footer(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "\n「/交辦 排程 暫停 編號」「恢復」「刪除」可以管理。",
        Lang::En => "\nManage with \"/assign schedules pause <id>\", \"resume\", \"remove\".",
        Lang::Ja => "\n「/交辦 排程 暫停 番号」「恢復」「刪除」で管理できます。",
    }
}

/// `next` / state for one list line.
pub fn state(lang: Lang, status: &str, pause_reason: Option<&str>, next: Option<&str>) -> String {
    match (lang, status) {
        (Lang::ZhTw, "paused") => format!("已暫停（{}）", pause_label(lang, pause_reason)),
        (Lang::En, "paused") => format!("paused ({})", pause_label(lang, pause_reason)),
        (Lang::Ja, "paused") => format!("一時停止中（{}）", pause_label(lang, pause_reason)),
        (Lang::ZhTw, "done") => "已執行完畢".into(),
        (Lang::En, "done") => "finished".into(),
        (Lang::Ja, "done") => "完了".into(),
        (Lang::ZhTw, _) => format!("下次 {}", next.unwrap_or("—")),
        (Lang::En, _) => format!("next {}", next.unwrap_or("—")),
        (Lang::Ja, _) => format!("次回 {}", next.unwrap_or("—")),
    }
}

fn pause_label(lang: Lang, reason: Option<&str>) -> &'static str {
    match (lang, reason.unwrap_or("")) {
        (Lang::ZhTw, "by_user") => "手動暫停",
        (Lang::ZhTw, "access_lost") => "建立者已沒有權限",
        (Lang::ZhTw, "agent_unavailable") => "AI 員工停用中",
        (Lang::ZhTw, "schedule_restricted") => "執行間隔不符合目前限制",
        (Lang::ZhTw, _) => "連續建立失敗",
        (Lang::En, "by_user") => "by you",
        (Lang::En, "access_lost") => "creator lost permission",
        (Lang::En, "agent_unavailable") => "AI staff disabled",
        (Lang::En, "schedule_restricted") => "repeat interval restricted",
        (Lang::En, _) => "repeated failures",
        (Lang::Ja, "by_user") => "手動",
        (Lang::Ja, "access_lost") => "作成者の権限なし",
        (Lang::Ja, "agent_unavailable") => "AI スタッフ停止中",
        (Lang::Ja, "schedule_restricted") => "実行間隔が制限対象",
        (Lang::Ja, _) => "作成の失敗が続いた",
    }
}

pub fn managed(lang: Lang, action: crate::assign_schedule::ManageAction, short_id: &str) -> String {
    use crate::assign_schedule::ManageAction as A;
    match (lang, action) {
        (Lang::ZhTw, A::Pause) => format!("已暫停排程 #{short_id}。"),
        (Lang::ZhTw, A::Resume) => format!("已恢復排程 #{short_id}。"),
        (Lang::ZhTw, A::Remove) => format!("已刪除排程 #{short_id}。"),
        (Lang::En, A::Pause) => format!("Paused schedule #{short_id}."),
        (Lang::En, A::Resume) => format!("Resumed schedule #{short_id}."),
        (Lang::En, A::Remove) => format!("Removed schedule #{short_id}."),
        (Lang::Ja, A::Pause) => format!("予約 #{short_id} を一時停止しました。"),
        (Lang::Ja, A::Resume) => format!("予約 #{short_id} を再開しました。"),
        (Lang::Ja, A::Remove) => format!("予約 #{short_id} を削除しました。"),
    }
}

pub fn manage_error(lang: Lang, e: &crate::assign_schedule::ManageError) -> &'static str {
    use crate::assign_schedule::ManageError as E;
    match (lang, e) {
        (Lang::ZhTw, E::NotFound) => "找不到這個排程編號（用「/交辦 排程」看你的排程）。",
        (Lang::ZhTw, E::NotApplicable) => "這個排程現在不能這樣做（可能已經是這個狀態）。",
        (Lang::ZhTw, E::CreatorLostAccess) => "你已經沒有交辦給這位 AI 員工的權限，不能恢復。",
        (Lang::ZhTw, E::TimePassed) => "這個一次性排程的時間已經過了，不能恢復。",
        (Lang::ZhTw, E::ScheduleRestricted) => {
            "執行間隔不符合目前的排程限制，請建立間隔較長的排程。"
        }
        (Lang::ZhTw, E::Store(_)) => "排程暫時無法處理，請稍後再試。",
        (Lang::En, E::NotFound) => "No such schedule (see \"/assign schedules\").",
        (Lang::En, E::NotApplicable) => {
            "That schedule can't do that right now (it may already be in that state)."
        }
        (Lang::En, E::CreatorLostAccess) => {
            "You no longer have permission to assign to this AI staff member, so it can't resume."
        }
        (Lang::En, E::TimePassed) => {
            "This one-time schedule's time has passed, so it can't resume."
        }
        (Lang::En, E::ScheduleRestricted) => {
            "The repeat interval no longer meets the scheduling limit. Create a schedule with a longer interval."
        }
        (Lang::En, E::Store(_)) => "Schedules are unavailable right now; please try again later.",
        (Lang::Ja, E::NotFound) => "その予約番号は見つかりません（「/交辦 排程」で確認できます）。",
        (Lang::Ja, E::NotApplicable) => {
            "その予約は今その操作ができません（すでにその状態かもしれません）。"
        }
        (Lang::Ja, E::CreatorLostAccess) => {
            "この AI スタッフに依頼する権限がないため、再開できません。"
        }
        (Lang::Ja, E::TimePassed) => "この1回だけの予約は時刻を過ぎたため、再開できません。",
        (Lang::Ja, E::ScheduleRestricted) => {
            "実行間隔が予約制限に適合しません。間隔を長くした予約を作成してください。"
        }
        (Lang::Ja, E::Store(_)) => "予約を処理できません。しばらくしてから再試行してください。",
    }
}

pub fn command_usage(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => {
            "用法：/交辦 排程（列出）、/交辦 排程 暫停 編號、/交辦 排程 恢復 編號、/交辦 排程 刪除 編號"
        }
        Lang::En => {
            "Usage: /assign schedules (list), /assign schedules pause <id>, resume <id>, remove <id>"
        }
        Lang::Ja => "使い方：/交辦 排程（一覧）、/交辦 排程 暫停 番号、恢復 番号、刪除 番号",
    }
}
