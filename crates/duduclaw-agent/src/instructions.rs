//! The `INSTRUCTIONS.md` prompt slot — one renderer shared by every prompt
//! assembly path (gateway channel replies, dispatch, minimal mode, the task
//! sandbox, and this crate's runner), so the heading and the minimal-mode cap
//! cannot drift between them.
//!
//! The file holds operating instructions an operator imported from another
//! platform (AGENTS.md / CLAUDE.md / GEMINI.md, rules directories, subagent
//! bodies). It is operator-managed: the agent-file guard refuses employee
//! writes to it exactly like SOUL.md.

/// Heading line of the rendered section.
pub const INSTRUCTIONS_HEADING: &str = "# Instructions";

/// Marker appended when minimal mode cut the file.
pub const TRUNCATED_MARKER: &str = "[truncated for minimal mode]";

/// Full-mode section: heading + the file verbatim (trailing whitespace
/// trimmed). `None` when the file is absent or blank.
pub fn render_section(instructions: Option<&str>) -> Option<String> {
    let body = instructions?.trim_end();
    if body.trim().is_empty() {
        return None;
    }
    Some(format!("{INSTRUCTIONS_HEADING}\n{body}"))
}

/// Minimal-mode section: same as [`render_section`] but the body is cut to
/// `max_bytes` on a char boundary (the same rule minimal mode applies to
/// SOUL.md), with [`TRUNCATED_MARKER`] when anything was dropped.
pub fn render_section_capped(instructions: Option<&str>, max_bytes: usize) -> Option<String> {
    let body = instructions?.trim_end();
    if body.trim().is_empty() {
        return None;
    }
    let cut = duduclaw_core::truncate_bytes(body, max_bytes);
    if cut.len() < body.len() {
        Some(format!(
            "{INSTRUCTIONS_HEADING}\n{cut}\n\n{TRUNCATED_MARKER}"
        ))
    } else {
        Some(format!("{INSTRUCTIONS_HEADING}\n{body}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_or_blank_renders_nothing() {
        assert!(render_section(None).is_none());
        assert!(render_section(Some("  \n\n")).is_none());
        assert!(render_section_capped(Some("\n"), 10).is_none());
    }

    #[test]
    fn full_section_is_verbatim_under_the_heading() {
        let s = render_section(Some("先讀 SOUL.md。\n\n")).unwrap();
        assert_eq!(s, "# Instructions\n先讀 SOUL.md。");
    }

    #[test]
    fn capped_section_cuts_on_a_char_boundary_and_says_so() {
        let body = "規則".repeat(100); // 600 bytes
        let s = render_section_capped(Some(&body), 10).unwrap();
        assert!(s.ends_with(TRUNCATED_MARKER), "{s}");
        // 10 bytes → three 3-byte chars, never half a char.
        assert!(s.starts_with("# Instructions\n規則規\n"), "{s}");
        let short = render_section_capped(Some("ok"), 10).unwrap();
        assert_eq!(short, "# Instructions\nok");
    }
}
