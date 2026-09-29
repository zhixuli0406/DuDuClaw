//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

use super::MethodHandler;

#[test]
fn github_repo_root_appends_skill_md() {
    assert_eq!(
        MethodHandler::resolve_skill_source_url("https://github.com/user/repo/").unwrap(),
        "https://raw.githubusercontent.com/user/repo/HEAD/SKILL.md"
    );
}

#[test]
fn github_blob_becomes_raw() {
    assert_eq!(
        MethodHandler::resolve_skill_source_url("https://github.com/u/r/blob/main/skills/x.md")
            .unwrap(),
        "https://raw.githubusercontent.com/u/r/main/skills/x.md"
    );
}

#[test]
fn github_tree_dir_appends_skill_md() {
    // The canonical way to share a subdirectory skill (Bug#1 fix).
    assert_eq!(
        MethodHandler::resolve_skill_source_url(
            "https://github.com/anthropics/skills/tree/main/skills/pptx"
        )
        .unwrap(),
        "https://raw.githubusercontent.com/anthropics/skills/main/skills/pptx/SKILL.md"
    );
    // Trailing slash tolerated.
    assert_eq!(
        MethodHandler::resolve_skill_source_url(
            "https://github.com/anthropics/skills/tree/main/skills/pptx/"
        )
        .unwrap(),
        "https://raw.githubusercontent.com/anthropics/skills/main/skills/pptx/SKILL.md"
    );
}

#[test]
fn github_tree_file_is_treated_like_blob() {
    // A tree URL that already names a file must not get SKILL.md appended.
    assert_eq!(
        MethodHandler::resolve_skill_source_url(
            "https://github.com/u/r/tree/main/skills/x/SKILL.md"
        )
        .unwrap(),
        "https://raw.githubusercontent.com/u/r/main/skills/x/SKILL.md"
    );
}

#[test]
fn gitlab_tree_dir_appends_skill_md() {
    assert_eq!(
        MethodHandler::resolve_skill_source_url(
            "https://gitlab.com/u/r/-/tree/main/skills/pptx"
        )
        .unwrap(),
        "https://gitlab.com/u/r/-/raw/main/skills/pptx/SKILL.md"
    );
}

#[test]
fn gist_gets_raw_suffix() {
    assert_eq!(
        MethodHandler::resolve_skill_source_url("https://gist.github.com/user/abc123").unwrap(),
        "https://gist.githubusercontent.com/user/abc123/raw"
    );
}

#[test]
fn gitlab_repo_and_blob() {
    assert_eq!(
        MethodHandler::resolve_skill_source_url("https://gitlab.com/u/r").unwrap(),
        "https://gitlab.com/u/r/-/raw/HEAD/SKILL.md"
    );
    assert_eq!(
        MethodHandler::resolve_skill_source_url("https://gitlab.com/u/r/-/blob/main/SKILL.md")
            .unwrap(),
        "https://gitlab.com/u/r/-/raw/main/SKILL.md"
    );
}

#[test]
fn direct_url_passes_through() {
    assert_eq!(
        MethodHandler::resolve_skill_source_url("https://example.com/skills/foo.md").unwrap(),
        "https://example.com/skills/foo.md"
    );
}

#[test]
fn lookalike_host_is_not_rewritten() {
    // Anchored host matching: github.com.evil.example must NOT be treated
    // as GitHub (no raw.githubusercontent rewrite).
    let out =
        MethodHandler::resolve_skill_source_url("https://github.com.evil.example/u/r").unwrap();
    assert_eq!(out, "https://github.com.evil.example/u/r");
}

#[test]
fn bad_scheme_rejected() {
    assert!(MethodHandler::resolve_skill_source_url("file:///etc/passwd").is_err());
    assert!(MethodHandler::resolve_skill_source_url("not a url").is_err());
}
