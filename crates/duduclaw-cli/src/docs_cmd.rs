//! `duduclaw docs [<topic>]` — Stripe-style docs entry point (E12).
//!
//! ## Why this shape
//!
//! DuDuClaw ships as a single compiled binary. The only thing embedded into
//! it today is the built dashboard SPA (`crates/duduclaw-dashboard/src/lib.rs`
//! — `#[derive(Embed)] #[folder = "dist/"]` via `rust-embed`); `scripts/release.sh`
//! has no step that bundles the `docs/` markdown tree into a release artifact,
//! and there is no `duduclaw-docs`-equivalent crate. So on a real user's
//! machine there is no local copy of `docs/features/*.md` /
//! `docs/guides/*.md` to read or render — a "topic" can only ever resolve to
//! a link, never to in-terminal rendered content. Given that constraint, the
//! only thing that can genuinely land is pointing the user at the
//! always-current docs that already live in the public repo on GitHub
//! (`docs/` is L1-Public, committed and published — see the root `CLAUDE.md`
//! doc-classification table), not a half-working local reader.
//!
//! Rather than hand-maintaining a *second* topic index that would inevitably
//! drift from reality, this reuses the index that already exists and is
//! already required to be kept in sync: `docs/README.md`'s "## Feature
//! Highlights" / "## User & Developer Guides" tables (project convention:
//! "Update docs/README.md ... whenever a docs/ file is added, moved, or
//! removed"). The file is embedded at compile time with the same
//! `include_str!` pattern this crate already uses for a markdown template
//! (`wizard.rs`'s `include_str!("../../../templates/wiki/CLAUDE_WIKI.md")`),
//! and parsed at runtime into a topic list — one source of truth, and the
//! parser itself is regression-tested against the live file (see the tests
//! module) so a format change in `docs/README.md` fails CI instead of
//! silently shipping an empty topic list.
//!
//! `duduclaw docs` alone lists every topic; `duduclaw docs <topic>` resolves
//! a slug/keyword match, prints the GitHub URL, and best-effort opens it in
//! the OS default browser. Opening is never required for the command to
//! succeed — this tool's most common deployment target is a headless server
//! (see `rules/gcp-deploy.md`), so "no browser available" is the *normal*
//! case, not a failure: the URL is always printed first regardless.

use duduclaw_core::error::{DuDuClawError, Result};

/// Embedded verbatim at compile time — see module docs for why this, and not
/// a hand-duplicated list, is the source of truth for available topics.
const DOCS_INDEX: &str = include_str!("../../../docs/README.md");

const REPO_DOCS_BASE: &str = "https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/";

/// One row parsed out of a `docs/README.md` table:
/// `| [text](path) | description | ... |`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DocTopic {
    /// Matching/display key — the linked file's name without its extension,
    /// e.g. `38-aee-playbook-evolution` or `evals`.
    slug: String,
    /// Repo-relative path under `docs/`, e.g.
    /// `features/38-aee-playbook-evolution.md`.
    path: String,
    description: String,
}

/// Parse every topic row inside the `## <heading>` section named exactly
/// `heading` (stops at the next `##` heading or EOF). Skips header/separator
/// rows and any row whose link points at an index page (`README.md`) — an
/// index is navigation, not a topic.
fn parse_section(markdown: &str, heading: &str) -> Vec<DocTopic> {
    let mut out = Vec::new();
    let mut in_section = false;
    for line in markdown.lines() {
        let trimmed = line.trim();
        if let Some(h) = trimmed.strip_prefix("## ") {
            in_section = format!("## {h}") == heading;
            continue;
        }
        if !in_section || !trimmed.starts_with('|') {
            continue;
        }
        if let Some(topic) = parse_table_row(trimmed) {
            out.push(topic);
        }
    }
    out
}

/// Parse one markdown table row into a [`DocTopic`], or `None` for a
/// header/separator/malformed row (never panics on a short/odd row — a
/// format hiccup in `docs/README.md` degrades to "this row is skipped", not
/// a crash).
fn parse_table_row(row: &str) -> Option<DocTopic> {
    let cells: Vec<&str> = row.trim_matches('|').split('|').map(str::trim).collect();
    let first = *cells.first()?;
    let description = cells.get(1).copied().unwrap_or("").to_string();
    // `[text](path)` — rejects the plain "Document" header cell and the
    // `---|---` separator row, neither of which is a markdown link.
    let after_bracket = first.strip_prefix('[')?;
    let (_text, rest) = after_bracket.split_once("](")?;
    let path = rest.strip_suffix(')')?.trim();
    if path.is_empty() || path.ends_with("README.md") {
        return None;
    }
    let filename = path.rsplit('/').next().unwrap_or(path);
    let slug = filename
        .strip_suffix(".md")
        .or_else(|| filename.strip_suffix(".html"))
        .or_else(|| filename.strip_suffix(".json"))
        .unwrap_or(filename)
        .to_string();
    Some(DocTopic { slug, path: path.to_string(), description })
}

/// Every `docs/features/` + `docs/guides/` topic — exactly the two sections
/// this command surfaces (spec/rfc/adr/todo/api docs are internal-dev-facing
/// reference material, not "topics" a channel/CLI user would ask about).
fn all_topics() -> Vec<DocTopic> {
    let mut out = parse_section(DOCS_INDEX, "## Feature Highlights");
    out.extend(parse_section(DOCS_INDEX, "## User & Developer Guides"));
    out
}

/// Resolve a user-typed keyword against the topic list: a case-insensitive
/// exact slug match wins outright; otherwise every slug/description
/// substring match is returned. An ambiguous keyword lists its candidates
/// rather than guessing which one the user meant.
fn resolve<'a>(topics: &'a [DocTopic], query: &str) -> Vec<&'a DocTopic> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Vec::new();
    }
    if let Some(exact) = topics.iter().find(|t| t.slug.to_lowercase() == q) {
        return vec![exact];
    }
    topics
        .iter()
        .filter(|t| t.slug.to_lowercase().contains(&q) || t.description.to_lowercase().contains(&q))
        .collect()
}

fn github_url(topic: &DocTopic) -> String {
    format!("{REPO_DOCS_BASE}{}", topic.path)
}

/// Best-effort open `url` in the OS default browser. Never surfaced as an
/// error — see module docs: a deployment with no display is the expected
/// common case for this tool, not a bug.
fn try_open_browser(url: &str) -> bool {
    // A Linux box with neither X11 nor Wayland has nothing `xdg-open` could
    // hand off to; skip the attempt outright rather than spawning a helper
    // that only fails.
    if cfg!(target_os = "linux")
        && std::env::var_os("DISPLAY").is_none()
        && std::env::var_os("WAYLAND_DISPLAY").is_none()
    {
        return false;
    }
    let status = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).status()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd").args(["/C", "start", "", url]).status()
    } else {
        std::process::Command::new("xdg-open").arg(url).status()
    };
    matches!(status, Ok(s) if s.success())
}

fn print_topic_list(topics: &[DocTopic]) {
    println!("📚 DuDuClaw 文件主題（輸入 `duduclaw docs <關鍵字>` 開啟指定文件）：");
    println!();
    for t in topics {
        println!("  {:<40} {}", t.slug, t.description);
    }
    println!();
    println!("完整索引：{REPO_DOCS_BASE}README.md");
}

/// `duduclaw docs [<topic>]`.
pub async fn run(topic: Option<String>) -> Result<()> {
    let topics = all_topics();
    if topics.is_empty() {
        // Should be unreachable in a real build — docs/README.md ships with
        // the repo this crate compiles from. Fail loud rather than silently
        // printing an empty list, which would look like "no docs exist"
        // instead of "the parser broke on a format change".
        return Err(DuDuClawError::Config(
            "duduclaw docs：找不到任何文件主題（docs/README.md 解析結果為空，索引格式可能已變更，請回報此問題）".into(),
        ));
    }

    let Some(query) = topic else {
        print_topic_list(&topics);
        return Ok(());
    };

    match resolve(&topics, &query).as_slice() {
        [] => {
            println!("⚠️ 找不到符合「{query}」的文件主題。");
            println!();
            print_topic_list(&topics);
        }
        [one] => {
            let url = github_url(one);
            println!("📖 {}", one.description);
            println!("🔗 {url}");
            if try_open_browser(&url) {
                println!("已嘗試在瀏覽器開啟。");
            } else {
                println!("無法自動開啟瀏覽器（可能是無圖形介面的伺服器環境），請手動複製上方連結開啟。");
            }
        }
        many => {
            println!("「{query}」符合多個主題，請輸入更精確的關鍵字：");
            println!();
            for t in many {
                println!("  {:<40} {}", t.slug, t.description);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = "\
# Fixture

## Feature Highlights

| Document | Description |
|----------|-------------|
| [features/README.md](features/README.md) | Feature index + full inventory |
| [features/01-foo.md](features/01-foo.md) | Foo feature — does foo things |
| [features/02-bar.md](features/02-bar.md) | Bar feature — does bar things |

## Format Specifications

| Document | Description | Status |
|----------|-------------|--------|
| [spec/x.md](spec/x.md) | Not a features/guides topic | Draft |

## User & Developer Guides

| Document | Description | Status |
|----------|-------------|--------|
| [guides/evals.md](guides/evals.md) | Agent behavior evals / regression suite | Current |

## API Reference

| Document | Description | Status |
|----------|-------------|--------|
| [api/README.md](api/README.md) | Should never appear (README) | Current |
";

    #[test]
    fn parses_only_the_named_section_and_skips_index_rows() {
        let features = parse_section(FIXTURE, "## Feature Highlights");
        assert_eq!(features.len(), 2, "{features:?}");
        assert_eq!(features[0].slug, "01-foo");
        assert_eq!(features[0].path, "features/01-foo.md");
        assert_eq!(features[0].description, "Foo feature — does foo things");
        assert_eq!(features[1].slug, "02-bar");

        let guides = parse_section(FIXTURE, "## User & Developer Guides");
        assert_eq!(guides.len(), 1, "{guides:?}");
        assert_eq!(guides[0].slug, "evals");
        assert_eq!(guides[0].path, "guides/evals.md");
    }

    #[test]
    fn parse_section_never_crosses_into_the_next_heading() {
        // "## Format Specifications" sits between the two sections we care
        // about; its one row must never leak into either.
        let features = parse_section(FIXTURE, "## Feature Highlights");
        assert!(!features.iter().any(|t| t.slug == "x"));
        let guides = parse_section(FIXTURE, "## User & Developer Guides");
        assert!(!guides.iter().any(|t| t.slug == "x"));
    }

    #[test]
    fn unmatched_heading_yields_nothing() {
        assert!(parse_section(FIXTURE, "## Does Not Exist").is_empty());
    }

    #[test]
    fn header_and_separator_rows_are_not_topics() {
        assert!(parse_table_row("| Document | Description |").is_none());
        assert!(parse_table_row("|----------|-------------|").is_none());
        assert!(parse_table_row("").is_none());
    }

    #[test]
    fn resolve_exact_slug_wins_over_substring_matches() {
        let topics = parse_section(FIXTURE, "## Feature Highlights");
        let hits = resolve(&topics, "01-foo");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].slug, "01-foo");
    }

    #[test]
    fn resolve_is_case_insensitive_and_matches_description_too() {
        let topics = parse_section(FIXTURE, "## Feature Highlights");
        assert_eq!(resolve(&topics, "FOO").len(), 1);
        assert_eq!(resolve(&topics, "bar things").len(), 1);
    }

    #[test]
    fn resolve_ambiguous_keyword_lists_every_candidate() {
        let topics = parse_section(FIXTURE, "## Feature Highlights");
        // Both fixture rows contain "feature" in their description.
        assert_eq!(resolve(&topics, "feature").len(), 2);
    }

    #[test]
    fn resolve_empty_query_matches_nothing() {
        let topics = parse_section(FIXTURE, "## Feature Highlights");
        assert!(resolve(&topics, "   ").is_empty());
    }

    #[test]
    fn github_url_is_built_under_the_docs_blob_path() {
        let t = DocTopic {
            slug: "evals".into(),
            path: "guides/evals.md".into(),
            description: String::new(),
        };
        assert_eq!(
            github_url(&t),
            "https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/evals.md"
        );
    }

    // ── Regression guard against the REAL docs/README.md ──────────────
    // If someone reshapes docs/README.md's table format, this fails loudly
    // in CI instead of `duduclaw docs` silently shipping an empty topic list.

    #[test]
    fn the_real_docs_readme_parses_into_a_non_trivial_topic_list() {
        let topics = all_topics();
        assert!(
            topics.len() > 20,
            "expected a healthy number of features+guides topics, got {}: {topics:?}",
            topics.len()
        );
        for t in &topics {
            assert!(
                t.path.starts_with("features/") || t.path.starts_with("guides/"),
                "topic leaked from an unexpected section: {t:?}"
            );
            assert!(!t.path.ends_with("README.md"), "index page leaked as a topic: {t:?}");
            assert!(!t.slug.is_empty());
            assert!(!t.description.is_empty(), "topic with no description: {t:?}");
        }
    }

    #[test]
    fn known_real_topics_resolve() {
        let topics = all_topics();
        assert!(!resolve(&topics, "evals").is_empty(), "guides/evals.md must resolve");
        assert!(!resolve(&topics, "playbook").is_empty(), "the AEE/playbook feature doc must resolve");
        assert!(!resolve(&topics, "goal-loop").is_empty(), "guides/goal-loop.md must resolve");
    }
}
