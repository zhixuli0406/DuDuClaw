# DuDuClaw 🐾

<div align="center">

[繁體中文](README.md) · **English** · [日本語](README.ja.md)

</div>

DuDuClaw turns AI command-line tools such as Claude Code, Codex and Antigravity into AI employees who actually deliver: they staff eleven messaging apps like Telegram, LINE, and Discord, an independent judge reviews their work before it ships, and every dollar they spend gets logged.

All you need is one Rust binary. Channel routing, conversation memory, multi-account rotation, behavioral guardrails, local inference, and a web dashboard are built in; swap the AI brain between twelve CLI backends (Claude Code, Codex, Antigravity, Grok and others; Gemini CLI is deprecated) or any OpenAI-compatible API whenever you like, and your config and memory stay on your own machine. The core is Apache 2.0.

[![CI](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml/badge.svg)](https://github.com/zhixuli0406/DuDuClaw/actions/workflows/ci.yml)
[![Version](https://img.shields.io/badge/version-1.68.1-blue)](https://github.com/zhixuli0406/DuDuClaw/releases)
[![npm](https://img.shields.io/npm/v/duduclaw?logo=npm)](https://www.npmjs.com/package/duduclaw)
[![PyPI](https://img.shields.io/pypi/v/duduclaw?logo=pypi)](https://pypi.org/project/duduclaw/)
[![License: Apache-2.0](https://img.shields.io/badge/License-Apache--2.0-blue.svg)](LICENSE)

https://github.com/user-attachments/assets/9f18408a-cf46-4db2-9ab0-dcc8db2486fc

## Table of contents

- [Why DuDuClaw?](#why)
- [Architecture at a glance](#architecture)
- [Prerequisites](#prerequisites)
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
| Telegram / LINE / Discord access | Terminal only | 11 channels, per-agent bot tokens |
| Multi-LLM failover | Manual restart | 4 rotation strategies + cross-provider failover |
| Context survives switching LLMs | Lost | Preserved |
| Conversation memory and knowledge base | Single session | SQLite temporal memory + layered wiki, auto-injected |
| Tools shared across LLMs | Rewrite per vendor | 249 MCP tools written once, reachable from the Claude, Codex, Gemini, Antigravity, Grok and OpenAI-compatible runtimes |
| Guardrails / audit / secret management | Build it yourself | Policy kernel + OS sandbox + AES-256-GCM built in |
| A whole box to hand to a customer | Install Linux yourself, manage updates and tamper resistance yourself | DuDuClaw OS image: A/B update with rollback + read-only root, plug in and go; a desktop shared by a person and the AI without getting in each other's way |

<a id="architecture"></a>

## Architecture at a glance

The AI runtime is the brain, DuDuClaw is the plumbing, and MCP (JSON-RPC 2.0) is the bridge. Swap the brain, keep the plumbing:

```
AI Runtime (brain) — Claude Code / Codex / Antigravity / Grok / … (12 CLIs) / OpenAI-compat
  ↕ MCP Protocol (JSON-RPC 2.0, stdin/stdout)
DuDuClaw (plumbing)
  ├─ Channel Router — Telegram / LINE / Discord / Slack / WhatsApp / Feishu
  │                    / Google Chat / Microsoft Teams / WeCom / DingTalk / WebChat
  ├─ Multi-Runtime — 13 runtime ids (12 CLIs + OpenAI-compat), auto-detected, per agent
  ├─ Session Memory — native --resume + temporal memory + key facts + layered wiki
  ├─ MCP Server — 249 tools (channels, memory, agents, skills, tasks, wiki, ERP)
  ├─ Evolution Engine — prediction-driven + AEE playbook rules + MistakeNotebook
  ├─ Security — PolicyKernel reference monitor + OS sandbox + redaction vault
  ├─ Inference Engine — OpenAI-compatible local server (llama-server / Ollama / vLLM) / llamafile
  ├─ Account Rotator — OAuth + API key rotation, budgets, health checks
  └─ Web Dashboard — React 19 SPA, embedded via rust-embed
```

The Rust workspace is 24 crates: the `duduclaw-core` foundation, the `duduclaw-gateway` service layer, the `duduclaw-llm` unified API layer, `duduclaw-inference` for local models, `duduclaw-memory` for cognitive memory, `duduclaw-security`, and more. Full design in [ARCHITECTURE.md](ARCHITECTURE.md).

Experimental calibrated local routing uses Varun Kotte's [UCCI (arXiv:2605.18796)](https://arxiv.org/abs/2605.18796). See [UCCI calibrated cascade](docs/features/57-ucci-calibrated-cascade.md) for configuration and data preparation.

The same gateway + dashboard also ships as a whole machine: [DuDuClaw OS](https://github.com/zhixuli0406/DuDuClaw-OS) is a Yocto-built appliance image. Its Yocto layer and release pipeline live in their own repo and vendor this repo's Rust workspace as a trimmed snapshot; see the Install section below.

<a id="prerequisites"></a>

## Prerequisites

DuDuClaw doesn't ship its own LLM — you need an AI brain first. Pick one (you can also set this up later in the browser wizard):

- Install one of the supported AI CLIs, such as [Claude Code](https://docs.anthropic.com/en/docs/claude-code), [Codex](https://github.com/openai/codex) or Antigravity (the full list is in [multi-runtime](docs/features/13-multi-runtime.md); [Gemini CLI](https://github.com/google-gemini/gemini-cli) is deprecated and removed in v1.69.0), and give it an API key. Anthropic and Google block consumer subscription tokens used by third-party products, and accounts have been suspended, so an API key is the supported path
- Bring an API key for any OpenAI-compatible provider
- Or run a local model behind an OpenAI-compatible server (llama-server, Ollama, vLLM, llamafile) — no cloud account needed

<a id="install"></a>

## Install

### Desktop app (recommended for personal use)

A native Tauri build that starts the local gateway automatically when you launch it — no terminal required — and shares `~/.duduclaw` with the CLI. Download from [Releases](https://github.com/zhixuli0406/DuDuClaw/releases):

| Platform | File | Notes |
|----------|------|-------|
| macOS (Apple Silicon / Intel) | `DuDuClaw_*.dmg` | Signed + Apple notarized, opens cleanly |
| Windows x64 | `DuDuClaw_*_x64_en-US.msi` | No Authenticode certificate yet, so SmartScreen warns; click "More info" then "Run anyway" to install — unblock details in [docs/guides/desktop-unblock.md](docs/guides/desktop-unblock.md) |
| Linux | `*_amd64.AppImage` / `.deb` | No signing needed |

> The macOS desktop build is available again from v1.68.1 (signed and notarized). v1.67.0 through v1.68.0 have no macOS build; Windows and Linux builds exist for those versions.

Open it and you're done — the in-app wizard walks you through picking an AI backend and creating your first agent, no commands needed.

### npm (advanced / server use, all platforms including Windows)

For running on a server, scripting automation, or if you just prefer the command line. The only prerequisite is [Node.js](https://nodejs.org/) 20+:

```bash
npm install -g duduclaw
```

This installs a prebuilt binary for your platform (macOS ARM64/x64, Linux x64/ARM64, Windows x64). No compiler, no Rust.

> ⚠️ If the install asks you for Rust / MSVC Build Tools and a 1.5-hour compile, you took a wrong turn. That path is "build from source" for contributors; regular users should use the npm command above.

### DuDuClaw OS (appliance image, pre-GA)

If you would rather not dedicate a computer, get a box that runs the moment you plug it in: [DuDuClaw OS](https://github.com/zhixuli0406/DuDuClaw-OS) is a Yocto-built Linux operating system in which the AI agent is a native resident: it boots into its own desktop (compositor / shell, lock screen, Cmd+K delegation bar), so a person and the AI share one x86-64 box without getting in each other's way — the agent's GUI work runs in a shadow workspace by default, and the moment you touch the keyboard or mouse, whatever it was driving on your desktop yields. The desktop edition ships with A/B atomic updates and rollback, a read-only root, and preloaded Chromium / LibreOffice / Steam plus a Chinese IME; Secure Boot signing, dm-verity and TPM2 are build-time overlay options that remain disabled as of the current v0.2.0 release, so boot with Secure Boot turned off in the BIOS/UEFI.

The current release is v0.2.0 (embedding platform v1.63.0; the prior v0.1.0 is kept as a rollback target). Download the whole-disk `.wic.zst` (desktop edition), the `installer-desktop` installer `.iso` (writes the desktop edition), or the plain `installer` `.iso` (writes the base image, without the app layer) from [DuDuClaw-OS Releases](https://github.com/zhixuli0406/DuDuClaw-OS/releases); every file comes with a `.sha256` and a minisign signature, and the verification commands are on [the OS README on the docs site](https://os.duduclaw.dudustudio.monster/docs/os/readme/). This is a bring-up line (0.x): QEMU-verified, **not yet booted on real hardware**. Hardware requirements and compatible machines: [docs/guides/hardware-requirements.md](docs/guides/hardware-requirements.md); product overview: [docs/features/50-duduclaw-os-appliance.md](docs/features/50-duduclaw-os-appliance.md).

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

- **Desktop app**: just open it — the gateway starts automatically and the wizard appears right inside the app.
- **npm / build from source**:

  ```bash
  duduclaw run                  # start everything (gateway + channels + scheduler + dispatcher)
  open http://localhost:18789   # open the dashboard
  ```

Either way, the first visit takes you through a wizard: pick an AI backend, create your first agent, then chat with it in the built-in WebChat — no need to run `duduclaw onboard` from a terminal first. Later, paste a bot token on the Channels page to put the same agent on Telegram, LINE, Discord, and the rest, without restarting.

Useful next steps:

```bash
duduclaw agent create      # create more agents
duduclaw wizard            # industry-template setup
duduclaw status            # system health snapshot
duduclaw update            # check for and install updates
duduclaw service install   # start on boot (launchd / systemd)
```

<a id="features"></a>

## Feature overview

| Area | What's built in | Read more |
|------|-----------------|-----------|
| Channels | 11 channels (Telegram / LINE / Discord / Slack / WhatsApp / Feishu / Google Chat / Teams / WeCom / DingTalk / WebChat), per-agent bots, hot start/stop, platform-native formatting, typing indicators, live task-progress boards; Telegram voice messages are transcribed through the OpenAI Whisper API. Discord voice channels are a non-default build feature and are not in the release binaries | [docs/features](docs/features/README.md) |
| Multi-runtime | 13 runtime ids: Claude Code / Codex / Antigravity / Grok / Qwen Code / Kimi Code / GitHub Copilot CLI / Kiro / Cursor / Mistral Vibe / OpenCode / Gemini CLI (deprecated, removed in v1.69.0) plus OpenAI-compat; auto-detected, per-agent config, context survives backend switches | [docs/features/13](docs/features/13-multi-runtime.md) |
| Unified LLM API layer | `duduclaw-llm` covers 4 native protocols (Anthropic Messages / OpenAI Responses / Gemini / OpenAI-compat) with one normalized request, plus 8 OpenAI-compat presets (DeepSeek / MiniMax / Groq / Together / Mistral / OpenRouter / xAI / Qwen), a pricing registry, and cross-provider fallback | [ARCHITECTURE.md](ARCHITECTURE.md) |
| MCP server | 249 tools: channels, memory, agent orchestration, skill market, task board, shared wiki, Odoo ERP, computer use, live forking; stdio and HTTP/SSE transports; an external client key gets a 7-tool baseline unless the operator grants it memory, wiki or messaging scopes, and connector, execution and admin tools are never exposed externally | [docs/api](docs/api/README.md) |
| Memory | SQLite temporal memory (fact supersession chains), HippoRAG-lite knowledge-graph retrieval (Personalized PageRank), Ebbinghaus forgetting-curve archival, cross-agent shared wiki | [docs/features](docs/features/README.md) |
| Self-evolution | Prediction-driven (by design most conversations end without an LLM call) + AEE playbook evolution: SOUL.md is read-only for agents, learned behavior is small rules that each link an eval case, a rule is committed only if it matches or beats the current playbook and is settled on its own after 24h; MistakeNotebook cross-turn memory | [evolution-engine.md](docs/architecture/evolution-engine.md) |
| Security | PolicyKernel reference monitor (zero-LLM, fail-closed), macOS Seatbelt / Linux Landlock native sandbox (per agent, off by default), container sandboxes (task sandbox: Docker only, off by default; script sandbox: Docker, with WSL2 tried first on Windows), secret redaction vault, CONTRACT.toml behavioral contracts + red-team CLI | [SECURITY.md](SECURITY.md) |
| Accounts and cost | OAuth + API key rotation (4 strategies), rate-limit and billing cooldowns, cost telemetry with cache-efficiency analytics. Each call runs the official CLI as a fresh process (the PTY session pool was removed in 2026-09; a one-shot pseudo-terminal remains for CLIs that require a TTY, such as Grok). Anthropic and Google block consumer subscription tokens in third-party products, so use API keys ([multi-runtime](docs/features/13-multi-runtime.md)) | [docs/features](docs/features/README.md) |
| Local inference | Any OpenAI-compatible local server (llama-server / Ollama / vLLM / SGLang) or llamafile, with three-tier confidence routing | [docs/features](docs/features/README.md) |
| Fine-tuning | Build SFT / DPO datasets (ShareGPT / Alpaca) from this machine's conversations, task results and approval decisions, train them on your own GPU host (SSH + LLaMA-Factory) or Together's cloud, then import the GGUF / LoRA back into the local models directory. No local training — integrated graphics cannot train — and data leaving the machine requires an explicit acknowledgement | [docs/features/54](docs/features/54-finetune.md) |
| Live forking | RFC-26: fork an in-progress task into N competing branches, each in a copy-on-write isolate, with an AI judge picking the winner to merge (off by default; in v1.67.0 do not enable it on Windows, see CHANGELOG) | [docs/rfc](docs/rfc) |
| Auto-update | One click from the dashboard or unattended (`auto_update = true`); SHA-256 + Ed25519 verification, in-place restart, open tabs reload themselves | [deployment-guide.md](docs/guides/deployment-guide.md) |
| Web dashboard | React 19 + TypeScript SPA embedded in the binary; zh-TW / en / ja | [docs/features](docs/features/README.md) |
| ERP | Odoo bridge with 17 MCP tools (CRM / sales / inventory / accounting), CE/EE auto-detection, per-agent credential isolation | [docs/rfc](docs/rfc/RFC-21-operator-guide.md) |
| DuDuClaw OS | Yocto appliance image (current release v0.2.0, embedding platform v1.63.0): own compositor / shell with keyboard shortcuts, human–AI co-driving (dedicated agent seat, shadow workspace, human input freezes the agent, Super+Esc emergency stop; compiled in, off by default), A/B atomic update with rollback, read-only root, first-boot provisioning + LAN dashboard, app compatibility layer (Flatpak / Bottles / Waydroid); Secure Boot signing / dm-verity / TPM2 are build overlay options (still not enabled as of v0.2.0); separate repo and version line, pre-GA | [docs/features/50](docs/features/50-duduclaw-os-appliance.md) · [52](docs/features/52-desktop-edition.md) |

Full feature list in [docs/features/feature-inventory.md](docs/features/feature-inventory.md); version history in [CHANGELOG.md](CHANGELOG.md).

<a id="cli"></a>

## CLI commands

```
duduclaw onboard             # first-run setup; the browser wizard covers this now, use for headless/scripted setups (--yes to skip prompts)
duduclaw run                 # start everything (gateway + channels + heartbeat + cron + dispatcher)
duduclaw agent               # interactive chat; subcommands create / list / inspect / pause / resume / run
duduclaw wizard              # industry-template setup
duduclaw status              # system health snapshot
duduclaw doctor              # diagnostics
duduclaw test <agent>        # red-team security test (9 built-in scenarios)
duduclaw eval                # run the agent behavior eval suite
duduclaw update              # check for and install updates
duduclaw service install     # install as a system service; also start / stop / status / logs / uninstall
duduclaw export / import     # export / import ~/.duduclaw (portable personal data)
duduclaw migrate from openclaw   # painless migration from OpenClaw / Hermes / paperclip (dry-run by default, --apply to write)
duduclaw mcp-server          # start the MCP server (stdio JSON-RPC 2.0)
duduclaw http-server         # start the MCP HTTP/SSE transport (Bearer auth)
duduclaw acp                 # start the Agent Client Protocol server (Zed / JetBrains / Neovim agent panels)
duduclaw acp server          # start the A2A server (agent-to-agent interop)
duduclaw license             # license management (activate / status / redeem / rebind / …)
```

Run `duduclaw --help` for the full command list and subcommands; developer topics are in the [development guide](docs/guides/development-guide.md).

<a id="trust"></a>

## Trust and security

What you install is fully transparent:

- **What's in the npm package**: a small JS wrapper plus platform binaries (`@duduclaw/<platform>` optionalDependencies). `postinstall` only checks that the platform package is present ([`install.js`](npm/duduclaw/scripts/install.js)); nothing is downloaded from arbitrary URLs or executed
- **No telemetry**: no usage data or conversation content is sent to us. The gateway checks GitHub Releases for updates every 6 hours; with a paid license installed it also refreshes the license with the license server (every 3–7 days depending on tier) and fetches the revocation list daily. Without a license file there is no licensing traffic. Secrets stay on your machine, encrypted with AES-256-GCM
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
| Channels | 11 | 25+ | 8 | 0 (API) |
| Multi-runtime | 13 runtime ids (12 CLIs + OpenAI-compat) | single | single | multi-LLM |
| MCP server | 249 tools | no | no | no |
| Self-evolution engine | AEE playbook rules (prediction-driven) | no | no | no |
| Local inference | OpenAI-compatible local server / llamafile + confidence routing | no | no | no |
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
- [docs/features/50-duduclaw-os-appliance.md](docs/features/50-duduclaw-os-appliance.md): the DuDuClaw OS appliance; [52-desktop-edition.md](docs/features/52-desktop-edition.md): the desktop edition, one machine shared by a person and the AI; hardware requirements in [hardware-requirements.md](docs/guides/hardware-requirements.md); app compatibility, image build and releases all live in the [DuDuClaw-OS](https://github.com/zhixuli0406/DuDuClaw-OS) repo (see its `docs/guides/app-compat.md`)
- [CHANGELOG.md](CHANGELOG.md): version history

<a id="license"></a>

## License

Open core: the core is [Apache License 2.0](LICENSE), free to use, modify, and distribute. Commercial add-ons (`commercial/`, not in this repository), such as the premium industry packs, are closed source and paid; a license key unlocks them. The license-verification client (`crates/duduclaw-license`) is part of the Apache 2.0 core. See [LICENSING.md](LICENSING.md).

<p align="center">
  🐾 Built with louis.li
</p>
