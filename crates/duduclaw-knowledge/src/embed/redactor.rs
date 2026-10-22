//! De-identification before any text leaves the machine.
//!
//! The cloud embedders call a [`TextRedactor`] on every passage and every
//! query before building a request. The gateway's implementation runs the
//! operator's redaction rules and replaces each hit with a fixed category
//! marker (`[EMAIL]`, `[TW_ID]`, …); nothing is restored afterwards, the
//! vector is computed over the marked text. A redactor that cannot run
//! (redaction off, no rules, rules failed to compile) returns `Err`, and
//! the embedder refuses to send — there is no bypass.

/// Turns text into the form that may be sent to a cloud provider.
pub trait TextRedactor: Send + Sync {
    /// The de-identified text, or why it cannot be produced (the text is
    /// then not sent).
    fn redact(&self, text: &str) -> Result<String, String>;
}

/// Replace byte ranges of `text` with `[CATEGORY]` markers. `spans` are
/// `(start, end, category)`, non-overlapping, on char boundaries; anything
/// else is refused rather than guessed.
pub fn mark_spans(text: &str, spans: &[(usize, usize, String)]) -> Result<String, String> {
    let mut sorted: Vec<&(usize, usize, String)> = spans.iter().collect();
    sorted.sort_by_key(|s| s.0);
    let mut out = String::with_capacity(text.len());
    let mut at = 0usize;
    for (start, end, category) in sorted {
        if *start < at || end < start || *end > text.len() {
            return Err("去識別化結果的位置不正確，未送出".to_string());
        }
        let (Some(head), Some(_)) = (text.get(at..*start), text.get(*start..*end)) else {
            return Err("去識別化結果的位置不正確，未送出".to_string());
        };
        out.push_str(head);
        out.push('[');
        out.push_str(&marker_name(category));
        out.push(']');
        at = *end;
    }
    out.push_str(text.get(at..).unwrap_or(""));
    Ok(out)
}

/// `email` → `EMAIL`, `tw-id` → `TW_ID`; anything outside `[A-Za-z0-9_-]`
/// is dropped so a category name cannot inject text.
fn marker_name(category: &str) -> String {
    let s: String = category
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .map(|c| {
            if c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect();
    if s.is_empty() {
        "REDACTED".to_string()
    } else {
        s
    }
}

/// A redactor that always refuses (no redaction configured).
#[derive(Debug, Clone)]
pub struct RefuseRedactor(pub String);

impl TextRedactor for RefuseRedactor {
    fn redact(&self, _text: &str) -> Result<String, String> {
        Err(self.0.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_become_category_markers() {
        let t = "聯絡 王小明 a@b.tw 謝謝";
        let s = t.find("a@b.tw").unwrap();
        let out = mark_spans(t, &[(s, s + "a@b.tw".len(), "email".into())]).unwrap();
        assert_eq!(out, "聯絡 王小明 [EMAIL] 謝謝");
        let p = t.find("王小明").unwrap();
        let out = mark_spans(
            t,
            &[
                (s, s + 6, "email".into()),
                (p, p + "王小明".len(), "person-name".into()),
            ],
        )
        .unwrap();
        assert_eq!(out, "聯絡 [PERSON_NAME] [EMAIL] 謝謝");
    }

    #[test]
    fn bad_spans_are_refused_not_guessed() {
        let t = "王小明";
        assert!(mark_spans(t, &[(1, 3, "x".into())]).is_err(), "mid-char");
        assert!(
            mark_spans(t, &[(0, 99, "x".into())]).is_err(),
            "out of range"
        );
        assert!(
            mark_spans(t, &[(0, 6, "x".into()), (3, 9, "y".into())]).is_err(),
            "overlap"
        );
        assert_eq!(marker_name("]\nignore"), "IGNORE");
    }
}
