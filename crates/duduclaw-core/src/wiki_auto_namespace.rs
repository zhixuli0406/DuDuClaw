//! The auto-filed wiki namespace (`agents/<id>/wiki/auto/`), judged the way
//! a file system may open a name (v1.84.0, RM-200).
//!
//! Pure string functions, no file-system access. They live here, below both
//! users, so the gateway (conversation visibility, MCP mutation refusals) and
//! the agent-file-guard hook (`org_field_guard::check_own_auto_wiki_write`)
//! fold names identically; `duduclaw_gateway::auto_wiki_page` re-exports
//! [`is_auto_namespace_segment`].

/// Top-level namespace of an employee wiki that only the gateway's auto
/// writer and the dashboard's human actions write.
pub const AUTO_NAMESPACE: &str = "auto";

/// Name prefix of the legacy in-wiki auto-page lock (`<wiki>/.auto-pages.lock`,
/// created by builds before the lock moved under `<home>/locks/`) and of any
/// sidecar beside it. The hook refuses employee writes to such a name.
pub const AUTO_PAGES_SIDECAR_PREFIX: &str = ".auto-pages";

/// Unicode characters some file systems ignore in a name (HFS+ drops them
/// when comparing names), so `au\u{200d}to/x.md` can open `auto/x.md`.
fn is_ignorable_in_names(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

/// One path segment as a file system may compare it: Unicode case-folded
/// (case-insensitive file systems), without characters a file system may
/// ignore, and without the trailing dots / spaces Win32 normalisation strips.
pub fn fold_segment(segment: &str) -> String {
    segment
        .chars()
        .filter(|c| !is_ignorable_in_names(*c))
        .collect::<String>()
        .trim_end_matches(['.', ' '])
        .to_lowercase()
}

/// Whether `segment` may open the directory entry `name` (lowercase ASCII)
/// on some supported file system ([`fold_segment`]).
pub fn segment_opens_as(segment: &str, name: &str) -> bool {
    fold_segment(segment) == name
}

/// v1.84.0 (review H2): whether one path segment names the auto namespace
/// the way a file system may open it — `AUTO/`, `auto./`, `auto /` and
/// `au\u{200d}to/` all open `auto/` somewhere. Errs towards "yes": callers
/// use it to hide pages and refuse writes.
pub fn is_auto_namespace_segment(segment: &str) -> bool {
    // ADS may name the directory itself; an 8.3 alias cannot be resolved
    // from text alone. The file guard must refuse either spelling too.
    segment.contains('~')
        || segment_opens_as(segment.split(':').next().unwrap_or(segment), AUTO_NAMESPACE)
}

/// Portable page-id components. Never use host-dependent `Path::components`
/// to classify an untrusted id. Unsafe paths have no identity; ordinary Unix
/// names retain their spelling. Namespace aliases are classified separately.
pub fn page_id_components(path: &str) -> Option<Vec<&str>> {
    if path.is_empty() || path.starts_with(['/', '\\'])
        || path.chars().any(char::is_control)
    {
        return None;
    }
    let mut parts = Vec::new();
    for part in path.split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || fold_segment(part).is_empty() {
            return None;
        }
        // Reject absolute and drive-relative prefixes even after leading `.`.
        let bytes = part.as_bytes();
        if parts.is_empty() && bytes.len() >= 2
            && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
        {
            return None;
        }
        parts.push(part);
    }
    (!parts.is_empty()).then_some(parts)
}

/// Canonical persistence key: `/` separators, no redundant `.` or separators.
/// Preserve component spelling: case/trailing-dot aliases must not turn an
/// unregistered Unix file into a registered file with the same content.
pub fn canonical_page_id(path: &str) -> Option<String> {
    Some(page_id_components(path)?.join("/"))
}

/// Conservative namespace classifier. Unclassifiable paths are protected,
/// including traversal and drive/UNC paths. Only the first component can
/// alias the auto namespace through ADS or a possible DOS short name.
pub fn is_auto_page_path(path: &str) -> bool {
    match page_id_components(path) {
        Some(parts) => is_auto_namespace_segment(parts[0]),
        None => true,
    }
}

/// Whether a file name in a wiki directory is the legacy auto-page lock or a
/// sidecar beside it (`.auto-pages`, `.auto-pages.lock`, `.AUTO-PAGES.tmp`, …).
pub fn is_auto_pages_sidecar(name: &str) -> bool {
    fold_segment(name).starts_with(AUTO_PAGES_SIDECAR_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_auto_ids_and_uncertain_paths_are_protected() {
        for path in [
            r"auto\sop\x.md", r"auto/sop\x.md", r".\auto\x.md",
            r"AUTO\x.md", r"auto\\x.md", r"a\..\auto\x.md",
            "./auto//x.md", "auto./x.md", "auto /x.md", "auto:stream/x.md",
            "AUTO~1/x.md", "a/../auto/x.md", "/auto/x.md", r"C:\auto\x.md", "", "../public.md",
        ] {
            assert!(is_auto_page_path(path), "{path:?}");
        }
        for (path, canonical) in [
            ("concepts/policy.md", "concepts/policy.md"),
            (r"concepts\policy.md", "concepts/policy.md"),
            ("automation/x.md", "automation/x.md"),
            ("auto.md", "auto.md"),
            ("entities/auto/x.md", "entities/auto/x.md"),
            ("notes/會議 10:30.md", "notes/會議 10:30.md"),
            ("entities/~draft.md", "entities/~draft.md"),
            ("notes/50%2e5.md", "notes/50%2e5.md"),
            ("notes%2farchive/x.md", "notes%2farchive/x.md"),
            ("notes%5carchive/x.md", "notes%5carchive/x.md"),
            ("notes:archive/x.md", "notes:archive/x.md"),
            ("notes./x.md", "notes./x.md"),
        ] {
            assert!(!is_auto_page_path(path), "{path:?}");
            assert_eq!(canonical_page_id(path).as_deref(), Some(canonical), "{path:?}");
        }
        for path in ["C:", r"c:\x.md", "C:x.md", "./C:/x.md", r"\public.md", ".", "notes/ /x.md", "notes/\u{200d}/x.md", "notes/\n.md"] {
            assert!(canonical_page_id(path).is_none(), "{path:?}");
            assert!(is_auto_page_path(path), "{path:?}");
        }
        assert_eq!(canonical_page_id(r".\auto//sop\x.md").as_deref(), Some("auto/sop/x.md"));
        assert!(canonical_page_id(r"a\..\auto\x.md").is_none());
        assert!(is_auto_namespace_segment("auto:stream"));
        assert!(is_auto_namespace_segment("AUTO~1"));
    }

    #[test]
    fn auto_namespace_segment_is_judged_as_opened() {
        for seg in ["auto", "AUTO", "Auto", "auto.", "auto ", "auto. .", "au\u{200d}to", "\u{feff}auto"] {
            assert!(is_auto_namespace_segment(seg), "{seg:?}");
        }
        for seg in ["automation", "autos", "auto.md", "_auto", "ａｕｔｏ", ""] {
            assert!(!is_auto_namespace_segment(seg), "{seg:?}");
        }
    }

    #[test]
    fn auto_pages_sidecars_are_recognised() {
        for name in [".auto-pages", ".auto-pages.lock", ".AUTO-PAGES.lock", ".auto-pages.lock.", ".auto\u{200b}-pages"] {
            assert!(is_auto_pages_sidecar(name), "{name:?}");
        }
        for name in ["auto-pages.lock", ".scope.toml", "_index.md"] {
            assert!(!is_auto_pages_sidecar(name), "{name:?}");
        }
    }
}
