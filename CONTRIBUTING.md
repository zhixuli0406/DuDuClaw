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
- **Commits**: [Conventional Commits](https://www.conventionalcommits.org/) (`feat:` / `fix:` / `docs:` …).
- **Definition of done**: builds + tests pass locally, behavior changes update the docs **in the same PR** (README/CHANGELOG/guides — stale docs are treated as bugs), and `CHANGELOG.md [Unreleased]` gets a human-readable entry.
- **Security gates fail closed**: if your change adds a permission/authz/parsing decision point, the unknown case must DENY. PRs that loosen a gate need an explicit rationale in the description.

## Docs contributions

Public docs live under `docs/` by type (`guides/`, `features/`, `spec/`, `rfc/`, `adr/`); update `docs/README.md`'s index in the same PR. Feature docs have `zh-TW/` and `ja-JP/` siblings — updating all three is appreciated but English-first PRs are fine.

## Reporting issues

Use GitHub Issues. For suspected security vulnerabilities, see `SECURITY.md` instead of filing a public issue.

## License

By contributing you agree your contribution is licensed under the repository's license (see `LICENSE`).
