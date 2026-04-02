---
name: release-version-bump-and-homebrew-formula-update
description: Workflow command scaffold for release-version-bump-and-homebrew-formula-update in DuDuClaw.
allowed_tools: ["Bash", "Read", "Write", "Grep", "Glob"]
---

# /release-version-bump-and-homebrew-formula-update

Use this workflow when working on **release-version-bump-and-homebrew-formula-update** in `DuDuClaw`.

## Goal

Synchronizes version numbers across Cargo.toml, Cargo.lock, README.md, and Homebrew formula (duduclaw.rb) for a new release.

## Common Files

- `Cargo.toml`
- `Cargo.lock`
- `HomebrewFormula/duduclaw.rb`
- `README.md`

## Suggested Sequence

1. Understand the current state and failure mode before editing.
2. Make the smallest coherent change that satisfies the workflow goal.
3. Run the most relevant verification for touched files.
4. Summarize what changed and what still needs review.

## Typical Commit Signals

- Update version in Cargo.toml and Cargo.lock
- Update version in HomebrewFormula/duduclaw.rb
- Update version references in README.md (and sometimes other docs)
- Commit with a message indicating version bump

## Notes

- Treat this as a scaffold, not a hard-coded script.
- Update the command if the workflow evolves materially.