# DuDuClaw for Chrome

Side-panel client for a self-hosted [DuDuClaw](https://github.com/zhixuli0406/DuDuClaw) gateway: chat with your AI employees, approve pending actions (HITL), and clip page selections into an agent's memory via the right-click menu.

## Install (unpacked, pre-store)

1. `chrome://extensions` → enable Developer mode → **Load unpacked** → pick this folder.
2. Open the extension options page: set your gateway URL and sign in with your dashboard email/password.
3. **One-time gateway setup**: extensions connect with an `Origin: chrome-extension://<id>` header. Add the bare extension id (shown on the options page, with a copy button) to `config.toml → [gateway] allowed_origins` or `DUDUCLAW_ALLOWED_ORIGINS`, then reload gateway config.

## Network & privacy disclosure

This extension communicates **only** with the gateway URL you configure. Loopback gateways (`127.0.0.1` / `localhost`) work out of the box; any other origin is requested as an optional host permission at login time. No telemetry, no analytics, no third-party requests. Tokens are stored in `chrome.storage.local`.

## License

Apache-2.0 (same as DuDuClaw).
