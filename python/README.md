# DuDuClaw Python SDK

Python companion package for [DuDuClaw](https://github.com/zhixuli0406/DuDuClaw) — the Multi-Agent AI Assistant Platform.

This package provides the Python modules that the DuDuClaw Rust binary calls via subprocess:

- **`duduclaw.evolution`** — Evolution vetter (GVU self-play verification)
- **`duduclaw.channels`** — Channel bridge base classes (Telegram, LINE, Discord)
- **`duduclaw.sdk`** — Claude Code SDK integration with multi-account rotation
- **`duduclaw.tools`** — Agent tool definitions

## Installation

```bash
pip install duduclaw
```

> **Note:** This package is a companion to the main DuDuClaw binary.
> Install the binary via Homebrew (`brew install zhixuli0406/tap/duduclaw`) or npm (`npx duduclaw`).

## Requirements

- Python 3.10+
- The `anthropic` and `httpx` packages (installed automatically)

## License

Apache-2.0
