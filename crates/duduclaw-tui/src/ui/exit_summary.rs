//! Plain, content-free output, printed only after the terminal guard is gone.
use crate::{state::Model, strings};
use std::path::Path;

pub(super) fn summary(model: &Model, home: &Path) -> String {
    let pending = model
        .inbox
        .count()
        .map(|n| n.to_string())
        .unwrap_or_else(|| "?".into());
    let running = if model.employees.count().is_none() {
        "?".into()
    } else {
        let employees: Vec<_> = model
            .employees
            .items
            .iter()
            .filter(|e| {
                e.status.is("working") || model.activity.get(e.id()).is_some_and(|a| a.working)
            })
            .collect();
        let mut names: Vec<_> = employees
            .iter()
            .take(5)
            .map(|e| {
                let name = if e.display_name.plain().is_empty() {
                    e.name_text()
                } else {
                    e.display_name.clone()
                };
                // Bound output, and never let a name add a physical output line.
                name.plain()
                    .replace('\n', " ")
                    .chars()
                    .take(80)
                    .collect::<String>()
            })
            .collect();
        if employees.len() > names.len() {
            names.push(strings::exit_more(employees.len() - names.len()));
        }
        if names.is_empty() {
            strings::EXIT_NONE.into()
        } else {
            names.join(", ")
        }
    };
    format!(
        "{}: {pending}\n{}: {running}\n{}: {}\n",
        strings::EXIT_PENDING,
        strings::EXIT_RUNNING,
        strings::EXIT_RETURN,
        resume_command(home)
    )
}

fn resume_command(home: &Path) -> String {
    // Bash ANSI-C quoting preserves arbitrary Unix path bytes without emitting
    // controls. Single quotes, backslashes and shell substitutions stay literal.
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let mut quoted = String::new();
        for byte in home.as_os_str().as_bytes() {
            match byte {
                b'\'' => quoted.push_str("\\'"),
                b'\\' => quoted.push_str("\\\\"),
                0x20..=0x7e => quoted.push(char::from(*byte)),
                _ => quoted.push_str(&format!("\\x{byte:02x}")),
            }
        }
        format!("DUDUCLAW_HOME=$'{quoted}' duduclaw tui")
    }
    #[cfg(not(unix))]
    {
        // PowerShell single-quoted segments and numeric UTF-16 code units work
        // on Windows PowerShell 5 as well as PowerShell 7.
        let parts: Vec<_> = home
            .to_string_lossy()
            .encode_utf16()
            .map(|c| format!("[char]{c}"))
            .collect();
        format!(
            "$env:DUDUCLAW_HOME = -join @({}); duduclaw tui",
            parts.join(",")
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitize::TerminalText;
    use serde_json::json;
    #[test]
    fn stale_summary_never_claims_zero_or_prints_content() {
        let mut model = Model::default();
        model.employees.items.push(serde_json::from_value(json!({"name":"a","display_name":"secret-content","status":"working","archived":false})).unwrap());
        let text = summary(&model, Path::new("/tmp/home"));
        assert!(text.contains(&format!("{}: ?", strings::EXIT_PENDING)));
        assert!(text.contains(&format!("{}: ?", strings::EXIT_RUNNING)));
        assert!(!text.contains("secret-content"));
        assert!(text.contains("duduclaw tui"));
    }
    #[test]
    fn fresh_summary_uses_only_sanitized_names_and_pending_count() {
        let mut model = Model::default();
        model.inbox.stale = false;
        model.inbox.refetch_pending = false;
        model.employees.stale = false;
        model.employees.refetch_pending = false;
        model.employees.items.push(serde_json::from_value(json!({"name":"a","display_name":"worker\u{1b}\u{202e}\nname","status":"working","archived":false})).unwrap());
        let text = summary(&model, Path::new("/tmp/a'$(touch nope)\n"));
        assert!(text.contains("worker name"));
        assert_eq!(text.lines().count(), 3);
        assert!(!text.chars().any(crate::sanitize::forbidden));
        assert!(text.contains(&format!("{}: 0", strings::EXIT_PENDING)));
        assert_eq!(TerminalText::new(text.clone()).plain(), text);
    }
}

#[cfg(test)]
mod bounds_tests {
    use super::*;
    #[test]
    fn thousands_of_workers_still_produce_a_short_summary() {
        let mut model = Model::default();
        model.employees.stale = false;
        model.employees.refetch_pending = false;
        for i in 0..2000 {
            model.employees.items.push(serde_json::from_value(serde_json::json!({
                "name":format!("worker-{i}"), "display_name":"worker", "status":"working", "archived":false
            })).unwrap());
        }
        let text = summary(&model, Path::new("/tmp/home"));
        assert!(text.contains(&strings::exit_more(1995)));
        assert!(text.len() < 300);
        assert_eq!(text.lines().count(), 3);
    }
}
