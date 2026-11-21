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
    segment_opens_as(segment, AUTO_NAMESPACE)
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
