//! RFC-26 §4.3 (P6.3): built-in skill set parity with deep-agents.
//!
//! Four bundled `SKILL.md` files installed into every new agent's `SKILLS/`
//! directory at creation, so a fresh agent has the default deep-agents toolbox:
//! `code-review`, `refactor`, `test-writer`, `git-workflow`. Idempotent: existing
//! files are never overwritten (operator edits win).

use std::path::Path;

/// `(skill_name, SKILL.md body)` for each bundled skill.
pub const BUILTIN_SKILLS: &[(&str, &str)] = &[
    (
        "code-review",
        "# Skill: code-review\n\n\
         Review a diff for correctness, security, and maintainability before it ships.\n\n\
         ## When to use\nAfter writing or modifying code, before committing.\n\n\
         ## Steps\n\
         1. Read the full diff (`git diff`), not just the latest hunk.\n\
         2. Check: correctness bugs, missing error handling, unvalidated input, secrets, \
         injection (SQL/shell/XSS), unsafe casts, race conditions.\n\
         3. Flag CRITICAL/HIGH issues first; note MEDIUM cleanups.\n\
         4. Prefer small, surgical fixes over rewrites.\n\n\
         ## Output\nA prioritized list: `severity · file:line · issue · suggested fix`.\n",
    ),
    (
        "refactor",
        "# Skill: refactor\n\n\
         Improve structure without changing behavior.\n\n\
         ## When to use\nCode is correct but hard to read, duplicated, or deeply nested.\n\n\
         ## Steps\n\
         1. Ensure tests exist and pass (lock in current behavior).\n\
         2. One transformation at a time: extract function, rename, dedupe, flatten nesting (>4 levels).\n\
         3. Keep functions <50 lines, files focused (<800 lines).\n\
         4. Re-run tests after each step; never mix refactor + behavior change in one commit.\n\n\
         ## Output\nA behavior-preserving diff plus a one-line rationale per change.\n",
    ),
    (
        "test-writer",
        "# Skill: test-writer\n\n\
         Write focused tests first (TDD), then minimal code to pass.\n\n\
         ## When to use\nNew feature, bug fix, or before refactoring untested code.\n\n\
         ## Steps\n\
         1. RED: write a failing test that states the desired behavior.\n\
         2. GREEN: minimal implementation to pass.\n\
         3. REFACTOR: clean up with tests green.\n\
         4. Cover edge cases: empty/None, boundaries, multi-byte/CJK input, error paths.\n\
         5. Target 80%+ coverage; one assertion theme per test.\n\n\
         ## Output\nTable-driven / parametrized tests with clear names describing the case.\n",
    ),
    (
        "git-workflow",
        "# Skill: git-workflow\n\n\
         Commit and open PRs cleanly.\n\n\
         ## When to use\nReady to commit or raise a pull request.\n\n\
         ## Steps\n\
         1. Never commit on the default branch — branch first.\n\
         2. Conventional commits: `<type>: <description>` (feat/fix/refactor/docs/test/chore/perf/ci).\n\
         3. PRs: analyze the FULL commit history (`git diff main...HEAD`), write a summary + test plan.\n\
         4. Confirm before any push or other hard-to-reverse action.\n\n\
         ## Output\nA conventional commit message, or a PR body with summary + test plan.\n",
    ),
];

/// Install the bundled skills into `skills_dir`, creating it if needed. Never
/// overwrites an existing `<name>/SKILL.md` (operator edits win). Returns the
/// names actually written.
pub fn install_builtin_skills(skills_dir: &Path) -> std::io::Result<Vec<&'static str>> {
    std::fs::create_dir_all(skills_dir)?;
    let mut written = Vec::new();
    for (name, body) in BUILTIN_SKILLS {
        // Anthropic Skills layout: <skills>/<name>/SKILL.md
        let dir = skills_dir.join(name);
        let file = dir.join("SKILL.md");
        if file.exists() {
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        std::fs::write(&file, body)?;
        written.push(*name);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundles_four_named_skills() {
        let names: Vec<&str> = BUILTIN_SKILLS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["code-review", "refactor", "test-writer", "git-workflow"]);
        // Every body is a non-trivial SKILL.md.
        assert!(BUILTIN_SKILLS.iter().all(|(_, b)| b.starts_with("# Skill:") && b.len() > 100));
    }

    #[test]
    fn install_writes_all_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("SKILLS");

        let first = install_builtin_skills(&skills).unwrap();
        assert_eq!(first.len(), 4);
        for (name, _) in BUILTIN_SKILLS {
            assert!(skills.join(name).join("SKILL.md").exists());
        }

        // Second run writes nothing (idempotent).
        let second = install_builtin_skills(&skills).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn install_does_not_overwrite_operator_edits() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("SKILLS");
        std::fs::create_dir_all(skills.join("refactor")).unwrap();
        std::fs::write(skills.join("refactor").join("SKILL.md"), "MY EDIT").unwrap();

        let written = install_builtin_skills(&skills).unwrap();
        assert!(!written.contains(&"refactor"));
        let body = std::fs::read_to_string(skills.join("refactor").join("SKILL.md")).unwrap();
        assert_eq!(body, "MY EDIT");
    }
}
