//! Pure helpers for keeping an agent's markdown identity files (`SOUL.md`,
//! `IDENTITY.md`) and its default `@trigger` in sync when `display_name`
//! changes via the dashboard `agents.update` RPC or the CLI MCP
//! `agent_update` tool.
//!
//! Root cause this fixes: renaming an agent only ever touched
//! `agent.toml`'s `display_name` field. The agent's system prompt self-name
//! comes 100% from the literal text burned into `SOUL.md` at creation time
//! (`# {display_name}` heading + `I am {display_name}, ...` sentence), so a
//! rename left the agent introducing itself with its old name forever.
//! Extracted as pure string functions (no I/O) so both the gateway RPC path
//! and the CLI MCP path can share + unit-test identical replacement logic.

/// Replace every exact occurrence of `old_name` with `new_name` in `content`.
///
/// Returns `(new_content, changed)`. A no-op (`changed = false`, content
/// returned unmodified) when:
/// - `old_name` is empty (nothing meaningful to search for),
/// - `old_name == new_name` (nothing would change), or
/// - `old_name` does not occur in `content`.
pub fn rename_in_markdown(content: &str, old_name: &str, new_name: &str) -> (String, bool) {
    if old_name.is_empty() || old_name == new_name || !content.contains(old_name) {
        return (content.to_string(), false);
    }
    (content.replace(old_name, new_name), true)
}

/// If `trigger` is exactly the auto-generated default pattern `@{old_name}`,
/// return the synced `@{new_name}` trigger. Otherwise `None` — a trigger the
/// operator customized away from the default is left untouched.
pub fn synced_trigger(trigger: &str, old_name: &str, new_name: &str) -> Option<String> {
    if old_name.is_empty() || old_name == new_name {
        return None;
    }
    if trigger == format!("@{old_name}") {
        Some(format!("@{new_name}"))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_replaces_all_occurrences() {
        let content = "# Alice\n\nI am Alice, a specialist AI agent. Alice loves cats.";
        let (out, changed) = rename_in_markdown(content, "Alice", "Bob");
        assert!(changed);
        assert_eq!(
            out,
            "# Bob\n\nI am Bob, a specialist AI agent. Bob loves cats."
        );
    }

    #[test]
    fn rename_no_occurrence_is_noop() {
        let content = "# Charlie\n\nI am Charlie.";
        let (out, changed) = rename_in_markdown(content, "Alice", "Bob");
        assert!(!changed);
        assert_eq!(out, content);
    }

    #[test]
    fn rename_empty_old_name_is_noop() {
        let content = "# Alice\n\nI am Alice.";
        let (out, changed) = rename_in_markdown(content, "", "Bob");
        assert!(!changed);
        assert_eq!(out, content);
    }

    #[test]
    fn rename_identical_names_is_noop() {
        let content = "# Alice\n\nI am Alice.";
        let (out, changed) = rename_in_markdown(content, "Alice", "Alice");
        assert!(!changed);
        assert_eq!(out, content);
    }

    #[test]
    fn synced_trigger_matches_default_pattern() {
        assert_eq!(
            synced_trigger("@Alice", "Alice", "Bob"),
            Some("@Bob".to_string())
        );
    }

    #[test]
    fn synced_trigger_leaves_custom_trigger_untouched() {
        assert_eq!(synced_trigger("@ali-bot", "Alice", "Bob"), None);
    }

    #[test]
    fn synced_trigger_empty_old_name_is_none() {
        assert_eq!(synced_trigger("@Alice", "", "Bob"), None);
    }

    #[test]
    fn synced_trigger_identical_names_is_none() {
        assert_eq!(synced_trigger("@Alice", "Alice", "Alice"), None);
    }
}
