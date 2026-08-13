# DuDuClaw for VS Code

Chat with your self-hosted [DuDuClaw](https://github.com/zhixuli0406/DuDuClaw) AI employees and approve their pending actions (HITL) without leaving the editor.

## Features

- 💬 **對話** — talk to your AI employees over the gateway's WebChat channel
- ✅ **審批** — see pending approval requests and approve/deny from the sidebar

## Setup

1. Run a DuDuClaw gateway (`duduclaw gateway`), local or remote.
2. Set `duduclaw.gatewayUrl` in VS Code settings (default `http://127.0.0.1:18789`).
3. Open the DuDuClaw sidebar (paw icon) and click **登入** — sign in with your dashboard email/password.

## Network & privacy disclosure

This extension communicates **only** with the DuDuClaw gateway URL you configure — typically a server on your own machine (`127.0.0.1`) or your own infrastructure. It makes no requests to any other host, collects **no telemetry**, and contains no analytics. Credentials are exchanged for a JWT via your gateway's `/api/login` and stored in VS Code Secret Storage; nothing is written to disk in plain text.

## License

Apache-2.0 (same as DuDuClaw).
