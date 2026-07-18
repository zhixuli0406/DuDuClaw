//! WP7 — company → department → personal knowledge/skill layering.
//!
//! A *department* is a lightweight grouping used to scope shared-wiki
//! namespaces (`shared/wiki/departments/<dept>/`) and shared skills
//! (`shared/skills/departments/<dept>/`). Departments are **derived** from the
//! `[agent] department` field in each agent.toml — there is no separate
//! registry. An agent with no department (empty / absent field) behaves
//! exactly as before WP7: it never sees any `departments/*` page or skill.
//!
//! This module holds the shared, dependency-free primitives (name validation +
//! path visibility) so every crate that touches the department tree — the CLI
//! MCP wiki tools, the gateway prompt-injection path, and the agent skill
//! loader — enforces one identical rule.

/// Top-level shared-wiki / shared-skill namespace that carries department
/// sub-trees. `departments/<dept>/<page>`.
pub const DEPARTMENTS_NAMESPACE: &str = "departments";

/// Validate a department identifier used as a filesystem path segment.
///
/// Denylist (not an ASCII allowlist): a name is valid when it is 1..=64 bytes,
/// is not `.`/`..`, and contains no path separator (`/`, `\`), NUL, control
/// character, or whitespace. This deliberately **allows** non-ASCII printable
/// Unicode so a zh-TW product can name a department "測試部" (Bug#5) while a
/// path built from a validated department still can never escape its parent dir
/// (no separators / traversal names get through).
///
/// The empty string is **invalid** on purpose: "no department" is a distinct
/// state that callers must handle *before* building any path — never by
/// passing `""` here.
pub fn is_valid_department(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || name == "." || name == ".." {
        return false;
    }
    !name.chars().any(|c| {
        c == '/' || c == '\\' || c == '\0' || c.is_control() || c.is_whitespace()
    })
}

/// The department that owns a shared-wiki page, if the page lives under the
/// built-in `departments/` namespace with the canonical
/// `departments/<dept>/<page>` shape. Returns `None` for pages outside the
/// namespace *and* for malformed department paths (e.g. a loose file directly
/// under `departments/`). Callers distinguish the two via
/// [`department_page_visible`], which fails closed on the malformed case.
pub fn department_of_page(page_path: &str) -> Option<&str> {
    let mut segs = page_path.split('/');
    if segs.next()? != DEPARTMENTS_NAMESPACE {
        return None;
    }
    let dept = segs.next()?;
    // Require a non-empty page component after the department segment.
    let page = segs.next()?;
    if dept.is_empty() || page.is_empty() {
        return None;
    }
    Some(dept)
}

/// Whether an agent whose department is `caller_department` may see/touch the
/// shared-wiki page at `page_path`.
///
/// - Pages **outside** the `departments/` namespace (the company layer) are
///   visible to everyone → `true`.
/// - Pages under `departments/<dept>/<page>` are visible only when the caller's
///   department exactly equals `<dept>` (coding convention #2 — exact equality,
///   never substring/prefix). An agent with no department (`None`) sees none.
/// - A path inside the `departments/` namespace that is *not* a well-formed
///   `departments/<dept>/<page>` (e.g. a loose `departments/foo.md`) is denied
///   for every agent — fail-closed.
pub fn department_page_visible(page_path: &str, caller_department: Option<&str>) -> bool {
    let mut segs = page_path.split('/');
    if segs.next() != Some(DEPARTMENTS_NAMESPACE) {
        // Company / other namespace — visible to all.
        return true;
    }
    match (segs.next(), segs.next()) {
        (Some(dept), Some(page)) if !dept.is_empty() && !page.is_empty() => {
            caller_department == Some(dept)
        }
        // departments/foo.md (no dept sub-dir) or departments/<dept>/ (no page).
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_department_allowlist() {
        // ASCII slugs and CJK/Unicode names are both accepted (Bug#5).
        for good in ["art", "sales", "eng-team", "team_2", "R2D2", "團隊", "測試部", "営業部"] {
            assert!(is_valid_department(good), "must accept {good:?}");
        }
        // Path-dangerous / whitespace / control shapes stay rejected.
        for bad in ["", "..", ".", "a/b", "a\\b", "a b", "團 隊", &"a".repeat(65), "nul\0", "tab\ttab", "new\nline"] {
            assert!(!is_valid_department(bad), "must reject {bad:?}");
        }
    }

    #[test]
    fn department_of_page_extracts_segment() {
        assert_eq!(department_of_page("departments/art/style.md"), Some("art"));
        assert_eq!(department_of_page("departments/art/sub/deep.md"), Some("art"));
        // Outside the namespace.
        assert_eq!(department_of_page("sop/deploy.md"), None);
        assert_eq!(department_of_page("faq.md"), None);
        // Malformed (loose file directly under departments/).
        assert_eq!(department_of_page("departments/foo.md"), None);
        assert_eq!(department_of_page("departments"), None);
    }

    #[test]
    fn company_pages_visible_to_everyone() {
        assert!(department_page_visible("sop/deploy.md", None));
        assert!(department_page_visible("sop/deploy.md", Some("art")));
        assert!(department_page_visible("faq.md", None));
    }

    #[test]
    fn department_pages_isolated_by_exact_department() {
        // Own department → visible.
        assert!(department_page_visible("departments/art/style.md", Some("art")));
        // Different department → hidden.
        assert!(!department_page_visible("departments/art/style.md", Some("sales")));
        // No department → hidden.
        assert!(!department_page_visible("departments/art/style.md", None));
        // Exact match only — no prefix leak.
        assert!(!department_page_visible("departments/art/style.md", Some("art-2")));
        assert!(!department_page_visible("departments/art-2/style.md", Some("art")));
    }

    #[test]
    fn malformed_department_path_is_fail_closed() {
        assert!(!department_page_visible("departments/foo.md", Some("foo")));
        assert!(!department_page_visible("departments/art/", Some("art")));
        assert!(!department_page_visible("departments", Some("art")));
    }
}
