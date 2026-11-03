# Contributing to DuDuClaw

Thanks for wanting to extend DuDuClaw! This page is the map — where each kind of contribution goes and what the quality bar is. 繁中使用者：各節開頭附中文摘要。

## What you can build (no fork required)

> 先看這張表：多數「擴充 DuDuClaw」的需求**不需要改核心程式碼**。

| You want to ship… | The unit | How | Docs |
|---|---|---|---|
| An AI employee / team (persona + skills + SOPs) | **Expert pack** (`expert.toml`) | `duduclaw expert pack` → share the zip/URL | [Build your own pack](docs/guides/build-your-own-pack.md) · [Reference](docs/features/32-expert-packs.md) |
| A reusable capability (prompt-driven) | **Skill** (`SKILL.md`) | Drop in `~/.duduclaw/skills/` or bundle inside a pack | [SKILL.md spec](docs/spec/skill-md-spec.md) |
| An integration with an external system | **External MCP server** (`[[mcp.external]]`) | Any MCP server mounts without forking — stdio or HTTP | [MCP bridge guide](docs/guides/mcp-bridge.md) |
| A starter template page in the gallery | Gallery entry (JSON) | Add an entry to `distribution/gallery/data/templates.json` | [Gallery README](distribution/gallery/README.md) |
| A client for another surface | Talk to the gateway APIs | WebChat WS / dashboard JSON-RPC / MCP HTTP+SSE | `clients/` (VS Code, Chrome, WordPress) as working examples |

**Security ground rule for all executable artifacts** (skills, hooks, MCP servers): declare what you touch. Packs with hooks install them **disabled** until the operator trusts them; skills pass the security scanner before activation; capability lists are deny-by-default. Don't fight these gates — they're why third-party artifacts are installable at all. (Three ecosystems learned this the hard way in 2026; we'd rather not be the fourth.)

## Code contributions

> 中文摘要：Rust workspace＋React dashboard；conventional commits；測試綠才算完成；文件與程式同 commit。

- **Setup**: `cargo build` (workspace), `cd web && npm install && npm run dev` for the dashboard. `cargo test -p <crate> --lib` for focused test runs.
- **Style**: match the file you're in. The repo is not rustfmt-clean — format only the lines you touch, never whole files. No unanchored `contains` for security/routing decisions; no raw byte-index string slicing (see `CLAUDE.md` → Coding Conventions).
- **Branches and PRs**: branch from `main` (`feat/…`, `fix/…`, `docs/…`, `chore/…`) and open the PR against `main`. Required checks are Rust on Ubuntu, Dashboard, Python and Security Audit. Inspect native macOS/Windows results before release.
- **CI scheduling**: PRs wait 60 seconds and cancel superseded revisions. PR Rust runs on Linux; pushes to `main`, nightly and manual runs retain the full three-platform suite and are not cancelled by later merges. The classifier runs from the PR base commit; missing or failed classification runs the full suite. Allowlisted documentation/web changes skip Rust (web still runs Dashboard); `docs/README.md` and `docs/assets/` require full coverage. Verified version-only release bumps run a locked workspace check. Other PRs run full Rust coverage, with Discord voice and app-compat checks selected by relevant paths. Python and Security Audit always run.
- **CI tests and profiles**: nextest runs the workspace and gateway suites; separate Cargo steps retain doctests, Windows acceptance cases and serial workflow E2E. Linux/PRs use `ci` with no retries; native macOS/Windows use `ci-native` with one retry. Both profiles report flaky tests, preserve separate JUnit artifacts and append flaky/failed names to the run summary. CI omits dev/test debug symbols while retaining debug assertions. Local build profiles are unchanged.
- **Release caches**: the Linux Rust job alone saves the cache shared with Dashboard. Release warming skips workspace-only lockfile version bumps; desktop warming saves only macOS caches. Tag container builds restore caches without exporting new tag-scoped blobs. Validate completed runner timings and cache usage before claiming a speedup or the 8.5 GB cache target.
- **Commits**: [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` / `docs:` …).
- **Definition of done**: builds + tests pass locally, behavior changes update the docs **in the same PR** (README/CHANGELOG/guides — stale docs are treated as bugs), and `CHANGELOG.md [Unreleased]` gets a human-readable entry.
- **Security gates fail closed**: if your change adds a permission/authz/parsing decision point, the unknown case must DENY. PRs that loosen a gate need an explicit rationale in the description.

## Docs contributions

Public docs live under `docs/` by type (`guides/`, `features/`, `spec/`, `rfc/`, `adr/`); update `docs/README.md`'s index in the same PR. Feature docs have `zh-TW/` and `ja-JP/` siblings — updating all three is appreciated but English-first PRs are fine.

## Reporting issues and asking questions

- **Bugs and feature requests**: open a GitHub Issue; the bug form asks for your version, install method and OS.
- **Questions, setup help, things you built**: use [Discussions](https://github.com/zhixuli0406/DuDuClaw/discussions).
- **Security vulnerabilities**: report them privately through [GitHub's vulnerability reporting](https://github.com/zhixuli0406/DuDuClaw/security/advisories/new); see `SECURITY.md`. Please do not open a public issue.
- **Want a first task?** Issues labelled [`good first issue`](https://github.com/zhixuli0406/DuDuClaw/labels/good%20first%20issue) are small and self-contained.

## Code of Conduct

This project follows the [Contributor Covenant](CODE_OF_CONDUCT.md). Report unacceptable behaviour to info@dudustudio.monster.

## License

By contributing you agree your contribution is licensed under the repository's license (see `LICENSE`).
