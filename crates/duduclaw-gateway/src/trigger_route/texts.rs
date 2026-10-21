//! Words the bot itself says around `@` calls on messaging channels, in the
//! language set by `config.toml` `[general] default_language` (zh-TW when
//! unset or unknown). WebChat sends machine codes instead and the dashboard
//! renders them in the viewer's language.

use std::path::Path;

/// A supported reply language.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    ZhTw,
    En,
    Ja,
}

/// The configured default language of this home.
pub fn lang_for(home: &Path) -> Lang {
    let Ok(text) = std::fs::read_to_string(home.join("config.toml")) else {
        return Lang::ZhTw;
    };
    let Ok(table) = text.parse::<toml::Table>() else {
        return Lang::ZhTw;
    };
    let code = table
        .get("general")
        .and_then(|g| g.get("default_language"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    parse_lang(code)
}

/// `en`, `en-US` → English; `ja`, `ja-JP` → Japanese; anything else zh-TW.
pub fn parse_lang(code: &str) -> Lang {
    let primary = code
        .split(['-', '_'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    match primary.as_str() {
        "en" => Lang::En,
        "ja" => Lang::Ja,
        _ => Lang::ZhTw,
    }
}

/// Calling an employee the sender may not call.
pub fn forbidden(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => {
            "你沒有呼叫這位 AI 員工的權限，這則訊息沒有送出。輸入「@」可以查看你能呼叫的 AI 員工。"
        }
        Lang::En => {
            "You don't have permission to call this AI staff member, so this message was not sent. Type \"@\" to see who you can call."
        }
        Lang::Ja => {
            "この AI スタッフを呼び出す権限がないため、このメッセージは送信されませんでした。「@」と入力すると呼び出せるスタッフを確認できます。"
        }
    }
}

/// The `@` list is empty.
pub fn nobody_callable(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "目前沒有你可以呼叫的 AI 員工。",
        Lang::En => "There is no AI staff member you can call right now.",
        Lang::Ja => "現在呼び出せる AI スタッフはいません。",
    }
}

/// Above a button list.
pub fn list_intro(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "要交給哪位 AI 員工？點一下名字，再說要做的事。",
        Lang::En => "Which AI staff member? Tap a name, then say what you need.",
        Lang::Ja => "どの AI スタッフに頼みますか？名前をタップしてから用件を送ってください。",
    }
}

/// Heading of a text list; `example` is one `@name`.
pub fn list_heading(lang: Lang, example: &str) -> String {
    match lang {
        Lang::ZhTw => format!("你可以呼叫的 AI 員工（輸入名稱前加 @，例如「{example} 幫我…」）："),
        Lang::En => {
            format!("AI staff you can call (type @ before the name, e.g. \"{example} please…\"):")
        }
        Lang::Ja => format!(
            "呼び出せる AI スタッフ（名前の前に @ を付けて入力。例：「{example} お願い…」）："
        ),
    }
}

/// After `@name` with no task.
pub fn ask_for_task(lang: Lang, name: &str, minutes: u64) -> String {
    match lang {
        Lang::ZhTw => format!(
            "請說要交給「{name}」的事，你的下一則訊息會直接交給他（{minutes} 分鐘內有效）。"
        ),
        Lang::En => format!(
            "What should {name} do? Your next message goes straight to them (valid for {minutes} minutes)."
        ),
        Lang::Ja => format!(
            "「{name}」への用件を送ってください。次のメッセージがそのまま届きます（{minutes} 分間有効）。"
        ),
    }
}

/// A LINE event that could not be handled after it was accepted.
pub fn please_resend(lang: Lang) -> &'static str {
    match lang {
        Lang::ZhTw => "剛剛那則訊息沒有處理到，請重新傳送一次。",
        Lang::En => "Your last message could not be handled. Please send it again.",
        Lang::Ja => "先ほどのメッセージを処理できませんでした。もう一度送ってください。",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_codes_map_to_the_three_supported_languages() {
        assert_eq!(parse_lang("en"), Lang::En);
        assert_eq!(parse_lang("en-US"), Lang::En);
        assert_eq!(parse_lang("ja-JP"), Lang::Ja);
        assert_eq!(parse_lang("zh-TW"), Lang::ZhTw);
        assert_eq!(parse_lang(""), Lang::ZhTw);
        assert_eq!(parse_lang("fr"), Lang::ZhTw);
    }

    #[test]
    fn missing_config_reads_as_zh_tw() {
        let t = tempfile::TempDir::new().unwrap();
        assert_eq!(lang_for(t.path()), Lang::ZhTw);
        std::fs::write(
            t.path().join("config.toml"),
            "[general]\ndefault_language = \"en\"\n",
        )
        .unwrap();
        assert_eq!(lang_for(t.path()), Lang::En);
    }
}
