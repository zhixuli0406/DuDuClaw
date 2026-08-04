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

/// Multi-file bundled skills (WP1.1 office document toolkit).
///
/// Each entry is `(skill_name, &[(relative_path, file_content)])`, where the
/// relative path is POSIX-style (`scripts/extract.py`) and content is embedded
/// at compile time via `include_str!`. This is the layout extension over the
/// single-file [`BUILTIN_SKILLS`]: a document skill ships a `SKILL.md` plus its
/// `scripts/*.py` helpers (PEP 723 inline-dependency Python driven by `uv run`).
///
/// The four office skills — `docx` / `xlsx` / `pptx` / `pdf` — are self-written
/// (no Anthropic source-available content) and teach the agent to read, create,
/// and convert documents, then hand the result back with the `📎DELIVER:`
/// marker the gateway understands.
pub const BUILTIN_SKILL_FILES: &[(&str, &[(&str, &str)])] = &[
    (
        "docx",
        &[
            ("SKILL.md", include_str!("builtin_skills/docx/SKILL.md")),
            ("scripts/extract.py", include_str!("builtin_skills/docx/scripts/extract.py")),
            ("scripts/create.py", include_str!("builtin_skills/docx/scripts/create.py")),
            ("scripts/to_pdf.py", include_str!("builtin_skills/docx/scripts/to_pdf.py")),
        ],
    ),
    (
        "xlsx",
        &[
            ("SKILL.md", include_str!("builtin_skills/xlsx/SKILL.md")),
            ("scripts/extract.py", include_str!("builtin_skills/xlsx/scripts/extract.py")),
            ("scripts/create.py", include_str!("builtin_skills/xlsx/scripts/create.py")),
            ("scripts/to_pdf.py", include_str!("builtin_skills/xlsx/scripts/to_pdf.py")),
        ],
    ),
    (
        "pptx",
        &[
            ("SKILL.md", include_str!("builtin_skills/pptx/SKILL.md")),
            ("scripts/extract.py", include_str!("builtin_skills/pptx/scripts/extract.py")),
            ("scripts/create.py", include_str!("builtin_skills/pptx/scripts/create.py")),
            ("scripts/to_pdf.py", include_str!("builtin_skills/pptx/scripts/to_pdf.py")),
        ],
    ),
    (
        "pdf",
        &[
            ("SKILL.md", include_str!("builtin_skills/pdf/SKILL.md")),
            ("scripts/extract.py", include_str!("builtin_skills/pdf/scripts/extract.py")),
            ("scripts/create.py", include_str!("builtin_skills/pdf/scripts/create.py")),
        ],
    ),
];

/// Install the bundled skills into `skills_dir`, creating it if needed. Never
/// overwrites a skill whose `<name>/SKILL.md` already exists (operator edits
/// win). Returns the names actually written. Installs both the single-file dev
/// skills ([`BUILTIN_SKILLS`]) and the multi-file office skills
/// ([`BUILTIN_SKILL_FILES`]).
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
    for (name, files) in BUILTIN_SKILL_FILES {
        let dir = skills_dir.join(name);
        // Idempotency gate keyed on SKILL.md — if the operator has the skill,
        // leave the whole directory (scripts included) untouched.
        if dir.join("SKILL.md").exists() {
            continue;
        }
        for (rel, content) in *files {
            // Build the destination by joining POSIX path components so nested
            // `scripts/foo.py` paths are portable across platforms.
            let mut dest = dir.clone();
            for part in rel.split('/') {
                dest.push(part);
            }
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&dest, content)?;
        }
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
    fn bundles_four_office_skills_with_scripts() {
        let names: Vec<&str> = BUILTIN_SKILL_FILES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["docx", "xlsx", "pptx", "pdf"]);
        // Every office skill ships a SKILL.md plus at least one script.
        for (_, files) in BUILTIN_SKILL_FILES {
            assert!(files.iter().any(|(rel, _)| *rel == "SKILL.md"));
            assert!(
                files.iter().any(|(rel, _)| rel.starts_with("scripts/") && rel.ends_with(".py")),
                "office skill missing scripts/*.py"
            );
            // Each script declares PEP 723 inline metadata for `uv run`.
            for (rel, content) in *files {
                if rel.ends_with(".py") {
                    assert!(content.contains("# /// script"), "{rel} missing PEP 723 header");
                }
            }
        }
    }

    #[test]
    fn install_writes_all_then_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("SKILLS");

        let first = install_builtin_skills(&skills).unwrap();
        // 4 single-file dev skills + 4 multi-file office skills.
        assert_eq!(first.len(), 8);
        for (name, _) in BUILTIN_SKILLS {
            assert!(skills.join(name).join("SKILL.md").exists());
        }
        // Office skills land with SKILL.md + scripts on disk.
        for (name, files) in BUILTIN_SKILL_FILES {
            assert!(skills.join(name).join("SKILL.md").exists());
            for (rel, _) in *files {
                let mut p = skills.join(name);
                for part in rel.split('/') {
                    p.push(part);
                }
                assert!(p.exists(), "missing {}/{rel}", name);
            }
        }

        // Second run writes nothing (idempotent).
        let second = install_builtin_skills(&skills).unwrap();
        assert!(second.is_empty());
    }

    #[test]
    fn office_skill_operator_edit_wins() {
        let dir = tempfile::tempdir().unwrap();
        let skills = dir.path().join("SKILLS");
        std::fs::create_dir_all(skills.join("docx")).unwrap();
        std::fs::write(skills.join("docx").join("SKILL.md"), "MY EDIT").unwrap();

        let written = install_builtin_skills(&skills).unwrap();
        assert!(!written.contains(&"docx"));
        assert_eq!(
            std::fs::read_to_string(skills.join("docx").join("SKILL.md")).unwrap(),
            "MY EDIT"
        );
        // But the scripts of an operator-edited skill are NOT force-written.
        assert!(!skills.join("docx").join("scripts").join("extract.py").exists());
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
