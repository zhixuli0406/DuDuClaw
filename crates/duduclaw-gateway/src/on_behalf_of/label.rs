//! How a principal is shown to people (approval cards, the goal prompt's
//! assigner line, audit views): a display name, never an account id where a
//! name exists.

use std::path::Path;

use duduclaw_core::HumanPrincipal;

/// "王小明（業務部）" for `user:<id>`, read from `users.db`. `None` for an
/// unmapped sender, a malformed value, or an account that cannot be read
/// or has no display name.
pub fn person_name(home: &Path, principal: &str) -> Option<String> {
    let user_id = match HumanPrincipal::parse(principal).ok()? {
        HumanPrincipal::User(id) => id,
        HumanPrincipal::Unmapped { .. } => return None,
    };
    let db = crate::decision_notify::open_user_db(home).ok().flatten()?;
    let user = db.get_user(&user_id).ok().flatten()?;
    let name = one_line(&user.display_name);
    if name.is_empty() {
        return None;
    }
    let dept = user.department.as_deref().map(one_line).filter(|d| !d.is_empty());
    Some(match dept {
        Some(dept) => format!("{name}（{dept}）"),
        None => name,
    })
}

/// [`person_name`], or a stand-in that still says who without exposing a
/// platform id: `user:<first 8 of the id>` for an account without a
/// readable name, 「未綁定帳號的 <channel> 使用者」 for an unmapped sender.
pub fn display_label(home: &Path, principal: &str) -> String {
    if let Some(name) = person_name(home, principal) {
        return name;
    }
    match HumanPrincipal::parse(principal) {
        Ok(HumanPrincipal::User(id)) => format!("user:{}", duduclaw_core::truncate_chars(&id, 8)),
        Ok(HumanPrincipal::Unmapped { channel, .. }) => format!("未綁定帳號的 {channel} 使用者"),
        Err(_) => "（無法辨識）".to_string(),
    }
}

/// One line of plain text, at most 40 characters: control characters (a
/// newline could start a fake instruction block), line/paragraph
/// separators, bidi controls and zero-width characters are dropped.
fn one_line(s: &str) -> String {
    let s: String = s.chars().filter(|&c| !is_hidden_or_breaking(c)).collect();
    duduclaw_core::truncate_chars(s.trim(), 40).to_string()
}

/// Characters a one-line display name must not carry: C0/C1 controls,
/// U+2028/U+2029, bidi embeddings, overrides and isolates (U+202A–U+202E,
/// U+2066–U+2069, U+200E/U+200F, U+061C), zero-width characters
/// (U+200B–U+200D, U+2060, U+FEFF) and the soft hyphen.
pub(crate) fn is_hidden_or_breaking(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{2028}' | '\u{2029}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2066}'..='\u{2069}'
                | '\u{200B}'..='\u{200F}'
                | '\u{061C}'
                | '\u{2060}'
                | '\u{FEFF}'
                | '\u{00AD}'
        )
}
