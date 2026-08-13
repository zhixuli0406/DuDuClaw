# DuDuClaw for Obsidian

Chat with the AI employees on your self-hosted [DuDuClaw](https://github.com/zhixuli0406/DuDuClaw) gateway from a right-panel view, and send any note into an agent's memory with one command.

## Features

- 💬 Right-panel chat (WebChat protocol, streaming replies)
- 🧠 Command: **把目前筆記存進 AI 員工記憶** — the agent files your note into its memory/knowledge base
- 🔐 Login with your dashboard email/password; only tokens are stored (never the password)

## Setup

1. Settings → DuDuClaw: gateway URL + Email → 登入.
2. **One-time gateway setup**: chat runs over WebSocket and Obsidian sends `Origin: app://obsidian.md` — add `obsidian.md` to `config.toml → [gateway] allowed_origins` and reload gateway config.

## Network & privacy disclosure (Obsidian developer policies)

This plugin connects **only** to the DuDuClaw gateway URL you configure — typically your own machine or server. No telemetry, no analytics, no third-party requests. Access/refresh tokens are stored in the plugin's data file inside your vault config; the password is used once per login and never persisted.

## Development

```bash
npm install && npm run build   # emits main.js next to manifest.json
```

Side-load: copy `manifest.json`, `main.js`, `styles.css` into `<vault>/.obsidian/plugins/duduclaw/`.

## Community plugin submission (👤)

PR to `obsidianmd/obsidian-releases` community-plugins.json（2026-05 起自動化掃描，天級上架）；先跑官方 `eslint-plugin` 檢查並保留本 README 的揭露段落。

## License

Apache-2.0 (same as DuDuClaw).
