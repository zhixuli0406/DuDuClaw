# DuDuClaw 🐾

<div align="center">

[繁體中文](README.md) · **English** · [日本語](README.ja.md)

</div>

DuDuClaw connects AI command-line tools like Claude Code, Codex, and Gemini to nine messaging platforms (Telegram, LINE, Discord, and more), turning them into an always-on AI assistant that remembers you and improves itself over time.

All you need is one Rust binary. Channel routing, conversation memory, multi-account rotation, behavioral guardrails, local inference, and a web dashboard are built in; swap the AI brain whenever you like, and your config and memory stay on your own machine. The core is Apache 2.0.

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-1.35.0-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

https://github.com/user-attachments/assets/9f18408a-cf46-4db2-9ab0-dcc8db2486fc

## Table of contents

- [Why DuDuClaw?](#why)
- [Architecture at a glance](#architecture)
- [Install](#install)
- [Quick start](#quickstart)
- [Feature overview](#features)
- [CLI commands](#cli)
- [Trust and security](#trust)
- [Comparison](#comparison)
- [Documentation](#docs)
- [License](#license)

<a id="why"></a>

## Why DuDuClaw?

If you run `claude` or `gemini` in a terminal now and then, the native CLIs are all you need. The moment you want an AI staffing your LINE official account, covering your team's Discord, or running several agents with different jobs, you end up building a whole infrastructure layer yourself. DuDuClaw ships that layer:

| Need | Native CLI | DuDuClaw |
|---|---|---|
| Telegram / LINE / Discord access | Terminal only | 9 channels, per-agent bot tokens |
| Multi-LLM failover | Manual restart | 4 rotation strategies + cross-provider failover |
| Context survives switching LLMs | Lost | Preserved |
| Conversation memory and knowledge base | Single session | SQLite temporal memory + layered wiki, auto-injected |
| Tools shared across LLMs | Rewrite per vendor | Write MCP tools once, use on all 5 backends |
| Guardrails / audit / secret management | Build it yourself | Policy kernel + OS sandbox + AES-256-GCM built in |

<a id="architecture"></a>

## Architecture at a glance

The AI runtime is the brain, DuDuClaw is the plumbing, and MCP (JSON-RPC 2.0) is the bridge:

```
AI Runtime (brain) — Claude Code / Codex / Gemini / Antigravity / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu
  │                    / Google Chat / Microsoft Teams / WebChat
  ├─ Multi-Runtime — 5 backends, auto-detected, configured per agent
  ├─ Session Memory — native --resume + temporal memory + key facts + layered wiki
  ├─ MCP Server — 80+ tools (channels, memory, agents, skills, tasks, wiki, ERP)
  ├─ Evolution Engine — GVU² dual-loop evolution + prediction-driven + MistakeNotebook
  ├─ Security — PolicyKernel reference monitor + OS sandbox + redaction vault
  ├─ Inference Engine — llama.cpp / mistral.rs / Exo P2P / llamafile / MLX
  ├─ Account Rotator — OAuth + API key rotation, budgets, health checks
  └─ Web Dashboard — React 19 SPA (24 pages), embedded via rust-embed
```

Full design in [ARCHITECTURE.md](ARCHITECTURE.md).

<a id="install"></a>

## Install

### npm (recommended, all platforms including Windows)

The only prerequisite is [Node.js](https://nodejs.org/) 20+:

```bash
npm install -g duduclaw
```

This installs a prebuilt binary for your platform (macOS ARM64/x64, Linux x64/ARM64, Windows x64). No compiler, no Rust.

> ⚠️ If the install asks you for Rust / MSVC Build Tools and a 1.5-hour compile, you took a wrong turn. That path is "build from source" for contributors; regular users should use the npm command above.

### Homebrew (macOS / Linux)

```bash
brew install zhixuli0406/tap/duduclaw
```

### One-line install

```bash
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.sh | sh
```

```powershell
# Windows (PowerShell)
irm https://raw.githubusercontent.com/zhixuli0406/DuDuClaw/main/scripts/install.ps1 | iex
```

### Desktop app

A native Tauri desktop build that starts the local gateway on launch and shares `~/.duduclaw` with the CLI. Download from [Releases](https://github.com/zhixuli0406/DuDuClaw/releases):

| Platform | File | Notes |
|----------|------|-------|
| macOS (Apple Silicon / Intel) | `DuDuClaw_*.dmg` | Signed + Apple notarized, opens cleanly |
| Windows x64 | `DuDuClaw_*_x64_en-US.msi` | No Authenticode certificate yet, so SmartScreen warns; click "More info" then "Run anyway", or use the CLI build instead |
| Linux | `*_amd64.AppImage` / `.deb` | No signing needed |

### Build from source

Prerequisites: [Rust](https://rustup.rs/) 1.85+, [Node.js](https://nodejs.org/) 20+.

```bash
git clone https://github.com/zhixuli0406/DuDuClaw.git
cd DuDuClaw
cd web && npm ci --legacy-peer-deps && npm run build && cd ..
cargo build --release -p duduclaw-cli -p duduclaw-gateway --features duduclaw-gateway/dashboard
./target/release/duduclaw run
```

### Python SDK (optional library)

The core gateway/CLI is a Rust binary and needs no Python. The `duduclaw` package on PyPI is a pure library for `import duduclaw` (agents / channels / mcp / memory_eval modules) with no command-line entry point, which is why `pipx install duduclaw` fails by design. If you need it:

```bash
pip install duduclaw
```

<a id="quickstart"></a>

## Quick start

You still need an AI brain, any one of these (you can also set it up later in the browser wizard): install and log in to [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex), or [Gemini CLI](https://github.com/google-gemini/gemini-cli); or bring an API key; or use a local GGUF model.

```bash
# 1. Start everything (gateway + channels + scheduler + dispatcher)
duduclaw run

# 2. Open the dashboard
open http://localhost:18789
```

The first visit walks you through a three-step wizard: pick an AI backend, create your first agent, then chat with it in the built-in WebChat. Later, paste a bot token on the Channels page to put the same agent on Telegram, LINE, Discord, and the rest, without restarting.

Useful next steps:

```bash
duduclaw agent create      # create more agents (industry templates available)
duduclaw status            # system health snapshot
duduclaw update            # check for and install updates
duduclaw service install   # start on boot (launchd / systemd)
```

<a id="features"></a>

## Feature overview

| Area | What's built in | Read more |
|------|-----------------|-----------|
| Channels | 9 channels (Telegram / LINE / Discord / Slack / WhatsApp / Feishu / Google Chat / Teams / WebChat), per-agent bots, hot start/stop, platform-native formatting, typing indicators, live task-progress boards | [docs/features](docs/features/README.md) |
| Multi-runtime | Claude / Codex / Gemini / Antigravity / OpenAI-compat, auto-detected, per-agent config, context survives backend switches | [ARCHITECTURE.md](ARCHITECTURE.md) |
| MCP server | 80+ tools: channels, memory, agent orchestration, skill market, task board, shared wiki, Odoo ERP; stdio and HTTP/SSE transports | [docs/api](docs/api/README.md) |
| Memory | SQLite temporal memory (fact supersession chains), Ebbinghaus retention, knowledge-graph retrieval, cross-agent shared wiki | [docs/features](docs/features/README.md) |
| Self-evolution | GVU² dual loop + prediction-driven (about 90% of conversations cost zero LLM calls), SOUL.md versioning with 24h observation and auto-rollback | [evolution-engine.md](docs/architecture/evolution-engine.md) |
| Security | PolicyKernel reference monitor (zero-LLM, fail-closed), macOS Seatbelt / Linux Landlock sandbox, secret redaction vault, CONTRACT.toml behavioral contracts + red-team CLI | [SECURITY.md](SECURITY.md) |
| Accounts and cost | OAuth + API key rotation (4 strategies), rate-limit and billing cooldowns, cost telemetry with cache-efficiency analytics | [docs/features](docs/features/README.md) |
| Local inference | llama.cpp (Metal/CUDA/Vulkan), mistral.rs, Exo P2P, llamafile, MLX, with three-tier confidence routing | [docs/features](docs/features/README.md) |
| Voice | ASR (SenseVoice / Whisper), TTS (Piper / MiniMax), VAD, Discord voice channels, LiveKit rooms | [docs/features](docs/features/README.md) |
| Auto-update | One click from the dashboard or unattended (`auto_update = true`); SHA-256 + Ed25519 verification, in-place restart, open tabs reload themselves | [deployment-guide.md](docs/guides/deployment-guide.md) |
| Web dashboard | React 19 SPA, 24 pages, embedded in the binary; zh-TW / en / ja | [docs/features](docs/features/README.md) |
| ERP | Odoo bridge with 15 MCP tools (CRM / sales / inventory / accounting), per-agent credential isolation | [docs/rfc](docs/rfc/RFC-21-operator-guide.md) |

Full feature list in [docs/features/feature-inventory.md](docs/features/feature-inventory.md); version history in [CHANGELOG.md](CHANGELOG.md).

<a id="cli"></a>

## CLI commands

```
duduclaw run                 # start everything (gateway + channels + heartbeat + cron + dispatcher)
duduclaw agent               # interactive chat in the terminal
duduclaw agent create        # create an agent (industry templates available)
duduclaw agent list          # list agents
duduclaw status              # system health snapshot
duduclaw doctor              # diagnostics
duduclaw test <agent>        # red-team security test (9 built-in scenarios)
duduclaw update              # check for and install updates
duduclaw service install     # install as a system service (launchd / systemd)
duduclaw mcp-server          # start the MCP server (stdio JSON-RPC 2.0)
duduclaw acp-server          # start the ACP/A2A server (Zed / JetBrains / Neovim)
```

Run `duduclaw --help` for the full list; developer topics are in the [development guide](docs/guides/development-guide.md).

<a id="trust"></a>

## Trust and security

What you install is fully transparent:

- **What's in the npm package**: a small JS wrapper plus platform binaries (`@duduclaw/<platform>` optionalDependencies). `postinstall` only checks that the platform package is present ([`install.js`](npm/duduclaw/scripts/install.js)); nothing is downloaded from arbitrary URLs or executed
- **No telemetry**: zero phone-home connections; all secrets stay on your machine, encrypted with AES-256-GCM
- **No privilege escalation**: runs entirely in user space
- **Maintainer**: DuDu Digital Technology Co., Ltd. (registered in Taiwan, tax ID 94139082)

Every release asset ships with three kinds of verification: a SHA-256 checksum, a [cosign](https://github.com/sigstore/cosign) keyless signature, and a minisign Ed25519 signature (the built-in auto-updater enforces this one and refuses unsigned or tampered releases):

```bash
# SHA-256
shasum -a 256 -c duduclaw-darwin-arm64.tar.gz.sha256

# minisign (the same public key is pinned inside the binary)
minisign -Vm duduclaw-darwin-arm64.tar.gz \
  -P RWTh5pOpk0YmdBgm3VyB2bzxFtajNLXr7zFDhbcc75TgM8YfeV+NSzXh
```

Don't trust prebuilt binaries? [Building from source](#install) takes three commands. Report vulnerabilities via [SECURITY.md](SECURITY.md).

> Why does a "new" package start at version 1.3x? DuDuClaw spent months in a private repo (400+ commits) before going public; the full history is in the [git log](https://github.com/zhixuli0406/DuDuClaw/commits/main).

<a id="comparison"></a>

## Comparison

| | DuDuClaw | OpenClaw | IronClaw | Dify |
|---|---|---|---|---|
| Language | Rust | TypeScript | Rust | Python |
| Channels | 9 | 25+ | 8 | 0 (API) |
| Multi-runtime | 5 backends | single | single | multi-LLM |
| MCP server | 80+ tools | no | no | no |
| Self-evolution engine | GVU² dual loop | no | no | no |
| Local inference | 6 backends + confidence routing | no | no | no |
| Behavioral contracts | CONTRACT.toml + red team | no | WASM sandbox | no |
| License | Apache 2.0 (open core) | MIT | open source | $59+/mo |

<a id="docs"></a>

## Documentation

- [ARCHITECTURE.md](ARCHITECTURE.md): full system architecture
- [docs/README.md](docs/README.md): public docs index (architecture / RFC / ADR / specs / guides)
- [docs/guides/deployment-guide.md](docs/guides/deployment-guide.md): production deployment (Tailscale / Docker / systemd / auto-update / monitoring)
- [docs/guides/development-guide.md](docs/guides/development-guide.md): dev environment and agent development
- [docs/guides/custom-mcp-tool.md](docs/guides/custom-mcp-tool.md): writing custom MCP tools
- [docs/spec](docs/spec/soul-md-spec.md): SOUL.md and CONTRACT.toml format specs
- [CHANGELOG.md](CHANGELOG.md): version history

<a id="license"></a>

## License

Open core: the core is [Apache License 2.0](LICENSE), free to use, modify, and distribute. Commercial add-on modules (`commercial/`) are closed source and paid, covering industry templates, the enterprise dashboard, and license verification. See [LICENSING.md](LICENSING.md).

<p align="center">
  🐾 Built with louis.li
</p>
