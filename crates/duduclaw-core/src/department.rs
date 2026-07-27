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
/// does not start with `.` (covers `.`/`..` and hidden dot-dirs), and contains
/// no path separator (`/`, `\`), NUL, control character, or whitespace. This deliberately **allows** non-ASCII printable
/// Unicode so a zh-TW product can name a department "測試部" (Bug#5) while a
/// path built from a validated department still can never escape its parent dir
/// (no separators / traversal names get through).
///
/// The empty string is **invalid** on purpose: "no department" is a distinct
/// state that callers must handle *before* building any path — never by
/// passing `""` here.
pub fn is_valid_department(name: &str) -> bool {
    if name.is_empty() || name.len() > 64 || name.starts_with('.') {
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

/// Extract the top-level namespace (first path segment) from a wiki-relative
/// `page_path`. A page directly at the wiki root (no `/`) lives in the
/// synthetic empty namespace `""`. Mirrors the CLI-side `top_level_namespace`
/// so the department read-visibility filter agrees across the gateway and CLI.
pub fn top_level_namespace(page_path: &str) -> &str {
    match page_path.split('/').next() {
        Some(seg) if !seg.is_empty() && seg != page_path => seg,
        _ => "",
    }
}

/// WP2.3 — decide whether a page in a namespace with an (optional)
/// `visible_to_departments` declaration is readable by `caller_department`.
///
/// - `allowed = None` → the namespace has no department declaration; no extra
///   restriction is imposed (existing behaviour preserved).
/// - `allowed = Some(list)` → **fail-closed**: only a caller whose department
///   is exactly one of `list` may read. A caller with no department (`None`)
///   is denied, and an empty list denies everyone (equivalent to
///   operator-only for reads). Matching is exact equality (coding convention
///   #2 — never substring/prefix).
pub fn namespace_department_visible(
    allowed: Option<&[String]>,
    caller_department: Option<&str>,
) -> bool {
    match allowed {
        None => true,
        Some(list) => match caller_department {
            Some(dept) => list.iter().any(|a| a == dept),
            None => false,
        },
    }
}

/// WP2.3 — the `visible_to_departments` read-visibility policy parsed from a
/// shared-wiki `.scope.toml`. This is **orthogonal to the write-mode policy**
/// (RFC-21 §3, which lives in the CLI `wiki_scope` module): a namespace may
/// declare both a write `mode` and a `visible_to_departments` read filter in
/// the same `[namespaces."x"]` table. Lives in `duduclaw-core` so the CLI MCP
/// wiki tools and the gateway prompt-injection path enforce one identical rule.
///
/// ## File shape
///
/// ```toml
/// [namespaces."hr"]
/// visible_to_departments = ["hr", "legal"]   # only hr/legal agents may read hr/*
/// ```
///
/// ## Fail-safe vs fail-closed
///
/// - Absent / malformed / unreadable `.scope.toml` → **empty policy**: every
///   namespace is visible (no extra restriction). Never blocks the gateway.
/// - A namespace that *is* declared → **fail-closed** per
///   [`namespace_department_visible`]: a caller with no department, or a
///   department not on the list, is denied.
#[derive(Debug, Clone, Default)]
pub struct DepartmentVisibilityPolicy {
    /// namespace → allowed departments. Absent key = visible to all.
    map: std::collections::BTreeMap<String, Vec<String>>,
}

impl DepartmentVisibilityPolicy {
    /// Empty policy — every namespace visible (deployment without a
    /// `visible_to_departments` declaration).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load from `<home_dir>/shared/wiki/.scope.toml`.
    pub fn load_for_home(home_dir: &std::path::Path) -> Self {
        Self::load_from(&home_dir.join("shared").join("wiki").join(".scope.toml"))
    }

    /// Load from `<wiki_dir>/.scope.toml`.
    pub fn load_for_wiki_dir(wiki_dir: &std::path::Path) -> Self {
        Self::load_from(&wiki_dir.join(".scope.toml"))
    }

    /// Load from an explicit `.scope.toml` path. Returns empty on any failure
    /// (absent / malformed / read error) — fail-safe, logged at WARN.
    pub fn load_from(path: &std::path::Path) -> Self {
        let raw = match std::fs::read_to_string(path) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Self::empty(),
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "reading .scope.toml for visible_to_departments");
                return Self::empty();
            }
        };
        Self::parse(&raw).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                ".scope.toml malformed — ignoring visible_to_departments (all namespaces visible)"
            );
            Self::empty()
        })
    }

    /// Parse the `visible_to_departments` map out of a `.scope.toml` body.
    /// Only namespaces that declare a `visible_to_departments` array contribute
    /// an entry; every other key in the table is ignored (write `mode`,
    /// `knowledge_owner`, `sensitivity`, …).
    pub fn parse(raw: &str) -> Result<Self, String> {
        let table: toml::Table = raw.parse().map_err(|e: toml::de::Error| e.to_string())?;
        let mut map = std::collections::BTreeMap::new();
        if let Some(namespaces) = table.get("namespaces").and_then(|v| v.as_table()) {
            for (ns, entry) in namespaces {
                if let Some(arr) = entry.get("visible_to_departments").and_then(|v| v.as_array()) {
                    let depts: Vec<String> = arr
                        .iter()
                        .filter_map(|v| v.as_str().map(|s| s.to_string()))
                        .collect();
                    map.insert(ns.clone(), depts);
                }
            }
        }
        Ok(Self { map })
    }

    /// Departments allowed to read `namespace`, or `None` when the namespace
    /// has no declaration (visible to all).
    pub fn allowed_for(&self, namespace: &str) -> Option<&[String]> {
        self.map.get(namespace).map(|v| v.as_slice())
    }

    /// Whether the namespace filter alone permits `caller_department` to read
    /// `page_path` (does NOT apply the built-in `departments/<dept>/`
    /// isolation — use [`Self::page_visible`] for the combined decision).
    pub fn namespace_allows(&self, page_path: &str, caller_department: Option<&str>) -> bool {
        let ns = top_level_namespace(page_path);
        namespace_department_visible(self.allowed_for(ns), caller_department)
    }

    /// The full read-visibility decision for a wiki page: **both** the built-in
    /// `departments/<dept>/` isolation ([`department_page_visible`]) **and** any
    /// `visible_to_departments` namespace filter must permit the caller.
    pub fn page_visible(&self, page_path: &str, caller_department: Option<&str>) -> bool {
        department_page_visible(page_path, caller_department)
            && self.namespace_allows(page_path, caller_department)
    }

    /// Whether any namespace declares a `visible_to_departments` filter.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// namespace → allowed departments, for status surfaces
    /// (`wiki_namespace_status`).
    pub fn snapshot(&self) -> &std::collections::BTreeMap<String, Vec<String>> {
        &self.map
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

    // ── WP2.3 visible_to_departments read-visibility ─────────────────────

    #[test]
    fn top_level_namespace_extracts_first_segment() {
        assert_eq!(top_level_namespace("hr/policy.md"), "hr");
        assert_eq!(top_level_namespace("a/b/c.md"), "a");
        assert_eq!(top_level_namespace("root.md"), "");
        assert_eq!(top_level_namespace(""), "");
    }

    #[test]
    fn namespace_visibility_undeclared_is_open() {
        // No declaration → visible to everyone, incl. no-department callers.
        assert!(namespace_department_visible(None, Some("hr")));
        assert!(namespace_department_visible(None, None));
    }

    #[test]
    fn namespace_visibility_declared_is_fail_closed() {
        let allowed = vec!["hr".to_string(), "legal".to_string()];
        // Listed department → visible.
        assert!(namespace_department_visible(Some(&allowed), Some("hr")));
        assert!(namespace_department_visible(Some(&allowed), Some("legal")));
        // Unlisted department → denied.
        assert!(!namespace_department_visible(Some(&allowed), Some("sales")));
        // No department → denied (fail-closed).
        assert!(!namespace_department_visible(Some(&allowed), None));
        // Exact match only — no prefix leak.
        assert!(!namespace_department_visible(Some(&allowed), Some("hr-2")));
        // Empty list denies everyone.
        assert!(!namespace_department_visible(Some(&[]), Some("hr")));
    }

    #[test]
    fn visibility_policy_parses_only_declared_namespaces() {
        let raw = r#"
            [namespaces."hr"]
            mode = "operator_only"
            visible_to_departments = ["hr", "legal"]

            [namespaces."sop"]
            mode = "agent_writable"
        "#;
        let p = DepartmentVisibilityPolicy::parse(raw).unwrap();
        assert!(!p.is_empty());
        assert_eq!(
            p.allowed_for("hr"),
            Some(&["hr".to_string(), "legal".to_string()][..])
        );
        // sop declares no visible_to_departments → open (None).
        assert_eq!(p.allowed_for("sop"), None);
    }

    #[test]
    fn visibility_policy_page_visible_combines_both_dimensions() {
        let raw = r#"
            [namespaces."hr"]
            visible_to_departments = ["hr"]
        "#;
        let p = DepartmentVisibilityPolicy::parse(raw).unwrap();

        // Declared namespace: only hr department reads hr/* pages.
        assert!(p.page_visible("hr/salary.md", Some("hr")));
        assert!(!p.page_visible("hr/salary.md", Some("sales")));
        assert!(!p.page_visible("hr/salary.md", None));

        // Undeclared company page: open to all.
        assert!(p.page_visible("sop/deploy.md", Some("sales")));
        assert!(p.page_visible("sop/deploy.md", None));

        // Built-in departments/ isolation still enforced independently.
        assert!(p.page_visible("departments/art/x.md", Some("art")));
        assert!(!p.page_visible("departments/art/x.md", Some("hr")));
    }

    #[test]
    fn empty_policy_never_restricts() {
        let p = DepartmentVisibilityPolicy::empty();
        assert!(p.is_empty());
        assert!(p.page_visible("hr/salary.md", None));
        assert!(p.page_visible("hr/salary.md", Some("sales")));
    }

    #[test]
    fn malformed_scope_toml_yields_empty_policy() {
        let dir = std::env::temp_dir().join(format!("dudu-vis-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(".scope.toml");
        std::fs::write(&path, "this = = not valid ===").unwrap();
        let p = DepartmentVisibilityPolicy::load_from(&path);
        assert!(p.is_empty(), "malformed file must fail-safe to empty");
        // Absent file → empty too.
        let p2 = DepartmentVisibilityPolicy::load_from(&dir.join("nope.toml"));
        assert!(p2.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
