//! Human-readable decision attribution; internal markers never become labels.
use crate::trigger_route::texts::Lang;

pub(crate) fn decider_display(
    marker: &str,
    name_of: impl Fn(&str) -> Option<String>,
    lang: Lang,
) -> Option<String> {
    let (user, source) = if let Some(user) = marker.strip_prefix("dashboard:") {
        if user.is_empty() || user.contains(':') { return None; }
        let source = match lang {
            Lang::ZhTw => "儀表板",
            Lang::En => "dashboard",
            Lang::Ja => "ダッシュボード",
        };
        (user.to_owned(), source.to_owned())
    } else {
        let by = marker.parse::<crate::approval::chat_admin::ledger::DecidedBy>().ok()?;
        let channel = match by.channel().as_str() {
            "telegram" => "Telegram",
            "googlechat" => "Google Chat",
            "line" => "LINE",
            "slack" => "Slack",
            "discord" => "Discord",
            _ => return None,
        };
        // The strict DecidedBy parser above validates the final step token.
        let method = match (lang, marker.rsplit(':').next()?) {
            (Lang::ZhTw, "tap") => "直接核准",
            (Lang::ZhTw, "web") => "網頁確認",
            (Lang::ZhTw, "totp") => "驗證碼確認",
            (Lang::En, "tap") => "one tap",
            (Lang::En, "web") => "web confirmation",
            (Lang::En, "totp") => "code confirmation",
            (Lang::Ja, "tap") => "ワンタップ",
            (Lang::Ja, "web") => "Web 確認",
            (Lang::Ja, "totp") => "コード確認",
            _ => return None,
        };
        let separator = match lang {
            Lang::ZhTw => "，",
            Lang::En => ", ",
            Lang::Ja => "、",
        };
        (by.user_id().to_owned(), format!("{channel}{separator}{method}"))
    };
    let name = duduclaw_core::plain_line(&name_of(&user)?, 80);
    if name.trim().is_empty() { return None; }
    Some(match lang {
        Lang::En => format!("{name} ({source})"),
        Lang::ZhTw | Lang::Ja => format!("{name}（{source}）"),
    })
}
