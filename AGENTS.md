# DuDuClaw Project Guidelines (Codex)

This file is loaded into every Codex session, so it stays short (target under 12 KB).
It repeats the rules from `CLAUDE.md` that apply to code changes; when you change one of
these sections, change it in both files. For anything not covered here, open the named
file and read only the section you need.

- Architecture map (crates, subsystems, where things live): `CLAUDE.md`, section "Architecture map".
- Current release state and deprecation policy: `CLAUDE.md`, section "Current state".
- UI and brand rules (dashboard work only): `CLAUDE.md`, section "Design Context".
- What shipped in which version: `CHANGELOG.md`.
- Per-subsystem history (file paths, config keys, known gaps): `wiki/duduclaw-kb/architecture/subsystem-reference.md`.
- Developer notes per release batch: `wiki/duduclaw-kb/architecture/release-implementation-notes.md`.

## Highest directive: every new feature and every fix gets its own branch

Owner instruction, 2026-10-03: 「每次有新功能與修復時都開分支」. It outranks every other workflow rule in this file.

- Before starting a feature or a fix, create a branch from the latest `origin/main` (`feat/…`, `fix/…`, `docs/…`, `chore/…`). Feature and fix commits never go on `main` directly.
- The change reaches `main` through a pull request. `main` requires four checks on a PR merge: Rust ubuntu-latest, Dashboard, Python, Security Audit (`cargo audit`, required since 2026-10-08). The ECC Tools app comments on PRs (security evidence, risk taxonomy and other audits); its results are advisory and not required checks.
- When another session is using the main checkout, work in a separate `git worktree` instead of switching branches there.
- When several open PRs touch the same files, merge them in the order the owner gives; the later one rebases.
- Release lanes: `minor`/`major` require the full signed delivery review; `hotfix` (deprecated alias: `patch`) auto-signs five unchanged delivery items and publishes all channels. Hotfix fixes and bump PRs use owner admin merge after `Rust ubuntu-latest` succeeds.
- Opening a branch is not permission to merge or release. Merging a PR and cutting a release stay the owner's decision.

## Cross-cutting rules

- A gate that cannot read its config, its file or its identity denies. Validate the artifact that takes effect, not the proposal.
- New settings and arguments may only narrow what an employee can do unless the owner decided otherwise; `allow` never removes a name-list gate.
- Anything an employee can list it can call, and anything it cannot call is not listed.
- Every spawn that hands an employee's `.mcp.json` to a CLI goes through the spawn gate; employees never write MCP or CLI config files.
- Facts that need the live gateway (`ApprovalBroker`, sessions, rotator) stay in the gateway; the separate `mcp-server` process reaches them through signed loopback routes or the shared databases.
- Write honest "not verified" notes. Live tests use an isolated home (`scripts/live-test/make-home.sh`, `mcp-probe.sh`) with a production-shaped employee (wildcard `mcp__duduclaw__*` allowlist).
- PR Rust CI runs on Linux; main pushes, nightly and manual runs test Linux, macOS and Windows as real legs. The release preflight blocks minor/major on missing or red native platform results and warns for hotfix. Discovery and computer-use workspaces are Unix-only by design.

## Coding Conventions (security-relevant, from 2026-06 deep review)

These conventions exist to prevent classes of bugs surfaced by the 2026-06 whole-project review. Shared helpers live in `duduclaw-core`.

1. **Never slice strings by raw byte index.** `&s[..n]` panics when `n` lands mid-char on multi-byte UTF-8 (any CJK/emoji input). Use `duduclaw_core::truncate_bytes(s, max_bytes)` (byte budget, walks back to a char boundary) or `truncate_chars(s, max_chars)` (codepoint count). Applies to every channel/log snippet and id prefix.
2. **No unanchored `contains` / `starts_with` for security or routing decisions.** Substring checks let `http://localhost.evil.com` pass a `localhost` allowlist and the keyword `hi` match inside `this`. Use `duduclaw_core::origin_host_matches(origin, &allowed)` for Origin/CORS allowlists and `word_contains_ci(haystack, needle)` for keyword matching; use exact/token equality for id routing.
3. **Cross-process file appends must hold an advisory lock.** Shared JSONL (`bus_queue.jsonl`, audit logs) is appended by both the Rust gateway and Python adapters; records over `PIPE_BUF` interleave and corrupt lines. Wrap the write in `duduclaw_core::with_file_lock(path, || …)`.
4. **Security gates fail closed.** Missing config, a detection error, or an unenumerated tool/method must DENY, never fall through to allow. MCP authorization is enumerated explicitly; any tool not in the scope table defaults to requiring Admin scope. Validate the *artifact that actually takes effect* (the written `patch.content`, the final screenshot), not just the proposal/narrative.

## Documentation Classification & Placement (from 2026-06-20 doc reorg)

Every document (`.md`, design notes, reports, specs) MUST be filed by **TYPE × CONFIDENTIALITY**. When creating or moving a doc, pick the matching cell below — do not drop loose files at a tree root, and never co-locate a higher-confidentiality doc inside a lower-confidentiality tree.

### Confidentiality tiers (3 levels)

| Tier | Meaning | Git state | Lives in |
|------|---------|-----------|----------|
| **L1 Public** | Product / developer docs intended for the public repo, website, npm/PyPI README | Committed, externally published | root `README*.md`, `CHANGELOG.md`, `ARCHITECTURE.md`, `SECURITY.md`, `LICENSING.md`, `docs/`, `templates/`, `marketing/press-kit/` |
| **L2 Internal** | Dev/ops material — committed but not product-facing | Committed (visible to repo collaborators), **not** polished docs | `wiki/` (knowledge base + reports), `team/`, `ISSUES.md`, `marketing/{blog,social-templates,threads-drafts}/` |
| **L3 Confidential** | Commercial plans, competitive analysis, pricing, research notes, secrets, build output | **gitignored — never committed to the public repo** | `commercial/` (own nested git), `research/`, `artifacts/`, `.env*`, anything under a secret manager |

**Hard rule:** L3 lives ONLY in gitignored trees. Before committing, confirm no `commercial/`, `research/`, pricing, or secret content is staged. `commercial/` and `research/` are in `.gitignore`; if `git ls-files <tree>` ever returns rows, untrack them with `git rm --cached -r <tree>` (files stay on disk).

### Type taxonomy → canonical home

**L1 Public — `docs/` only (one subdir per type, never loose at `docs/` root):**
- `docs/architecture/` — system architecture & engine design (`overview.md`, `evolution-engine.md`)
- `docs/rfc/` — Request-for-Comments proposals (`RFC-NN-*.md`, plus their operator guides)
- `docs/adr/` — Architecture Decision Records (`ADR-NNN-*.md`)
- `docs/todo/` — public planning / tracking (`TODO-*.md`)
- `docs/spec/` — open format specs (SOUL.md, CONTRACT.toml, JSON Schema)
- `docs/features/` — feature deep-dives (+ `ja-JP/`, `zh-TW/` translations)
- `docs/guides/` — user & developer how-tos (deployment, development, docker, MCP tools)
- `docs/api/` — protocol / OpenAPI reference

**L2 Internal — operational, under `wiki/` (single report home) + `team/`:**
- `wiki/reports/tl-daily/<YYYY-MM-DD>.md` — daily reports (date-only filename; **never** `daily-report-*` and **never** in `docs/`, `reports/`, or `artifacts/`)
- `wiki/reports/skill-pipeline/<YYYY-MM>/`, `wiki/reports/memory-quality/<YYYY-MM>/` — automated pipeline/eval outputs
- `wiki/sprint-reports/`, `wiki/pm/` — sprint logs & PM tracking
- `wiki/duduclaw-kb/{architecture,decisions,specs,drafts,w19,sprint-n}/` — internal knowledge base
- `wiki/{eval,impl,research}/` — evaluation sets, implementation notes, internal design feedback
- `team/` — team roster & role docs

**L3 Confidential — gitignored trees (do NOT add to `docs/` or `wiki/`):**
- `commercial/docs/` — business plans, pricing, competitive analysis, SI methodology, commercial design docs (e.g. PTY-pool design, runtime TODOs)
- `research/` — AI paper / repo research notes (auto-mirrored to the shared wiki at runtime)
- `artifacts/` — build output (tarballs, generated bundles)

### When in doubt
1. **Is it secret / commercial / competitive / a research note?** → L3 gitignored tree. Stop.
2. **Is it a daily/sprint/eval/PM report or internal KB?** → L2 under `wiki/` (reports → `wiki/reports/…`).
3. **Is it polished product/dev documentation safe to publish?** → L1 under the matching `docs/<type>/` subdir.
4. Update `docs/README.md` (the public index) whenever a `docs/` file is added, moved, or removed; update in-repo links (READMEs, `CLAUDE.md`, code comments, `CONTRACT.toml`) in the same commit.
