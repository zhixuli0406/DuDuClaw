# DuDuClaw

DuDuClaw connects AI coding tools to messaging apps and provides a local dashboard for managing AI staff, tasks and conversation memory. The core is Apache-2.0, maintained by Dudu Technology Ltd.

This npm package is a JavaScript launcher for the prebuilt Rust binary. It selects the optional binary package for your platform; no Rust compiler is needed. Node.js 20 or newer is required.

## Install and run

```sh
npm install -g duduclaw
duduclaw run
```

Open http://localhost:18889 and follow the setup wizard. Bring an AI provider API key, a supported CLI, or a local OpenAI-compatible model server. Provider usage may incur charges. Subscription sign-in has provider-specific terms and a risk notice; it is not a promise of unlimited or included usage.

## Standalone MCP memory server

```sh
npx duduclaw mcp init --client claude-code
```

This initializes local memory and client configuration without starting the full gateway. Follow the command's output to complete setup. See the [standalone MCP guide](https://github.com/zhixuli0406/DuDuClaw/blob/main/docs/guides/mcp-standalone.md) for authentication, namespaces and other supported clients.

## Package contents and support

The wrapper contains the launcher and install check. Platform binaries are delivered through the matching `@duduclaw/*` optional dependency. See the [source and full README](https://github.com/zhixuli0406/DuDuClaw), [documentation](https://duduclaw.dudustudio.monster/docs/), [releases](https://github.com/zhixuli0406/DuDuClaw/releases), and [issue tracker](https://github.com/zhixuli0406/DuDuClaw/issues).

## License

Apache-2.0. Commercial add-ons have separate terms; see [LICENSING.md](https://github.com/zhixuli0406/DuDuClaw/blob/main/LICENSING.md).
