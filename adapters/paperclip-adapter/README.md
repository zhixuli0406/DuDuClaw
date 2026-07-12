# @duduclaw/paperclip-adapter

Paperclip external adapter that runs [DuDuClaw](https://github.com/dudustudio/duduclaw) agents through the local `duduclaw` binary. Install it in a paperclip host and its companies can assign work to agents that live in DuDuClaw — with their own SOUL.md, skills, memory, and account rotation.

## How it works

- The adapter never runs models itself. It talks to `duduclaw mcp-server` over stdio JSON-RPC: `send_to_agent` enqueues the task, then `check_responses` is polled until the agent's reply lands.
- Task execution therefore requires a running DuDuClaw gateway on the same machine (`duduclaw run` or `duduclaw service start`) — the gateway dispatcher is what actually executes the agent.
- Agent discovery uses `duduclaw agent list --json` (available since duduclaw v1.36).

## Install

In paperclip: Settings → Adapters → Install from npm → `@duduclaw/paperclip-adapter`, or:

```
POST /api/adapters
{"packageName": "@duduclaw/paperclip-adapter"}
```

Prerequisites: Node 18+, a local DuDuClaw install with at least one agent (`duduclaw agent list --json` to verify), and the gateway running.

## Agent configuration

Set the adapter type to `duduclaw` and configure per agent:

| key | required | default | meaning |
| --- | --- | --- | --- |
| `agentId` | yes | — | DuDuClaw agent id |
| `binaryPath` | no | `duduclaw` | absolute path to the binary |
| `duduclawHome` | no | `~/.duduclaw` | sets `DUDUCLAW_HOME` for the spawned processes |
| `promptTemplate` | no | — | `{{variable}}` template rendered against the run context |
| `timeoutMs` | no | `300000` | max wait for the agent response |
| `pollIntervalMs` | no | `2000` | response poll interval |

Example `.paperclip.yaml` entry (this is exactly what `duduclaw export --format agentcompanies` emits):

```yaml
schema: paperclip/v1
agents:
  boss:
    adapter:
      type: duduclaw
      config:
        agentId: "boss"
        model: "claude-sonnet-4-6"
```

## Development

```sh
npm install
npm run build
npm test
```

Tests run against a mocked `duduclaw` binary (`test/fixtures/fake-duduclaw.mjs`); no DuDuClaw install is needed to develop the adapter.

## Known limitations

- `check_responses` previews responses at 500 characters; longer replies are truncated in the transcript (`[truncated, full=N chars]` marker). Full output stays available on the DuDuClaw side (session store, channels).
- Session continuity across runs maps to DuDuClaw's own session manager; the adapter only records the last dispatched message id in `sessionParams`.
- The adapter contract types are transcribed locally from the paperclip adapter docs rather than imported from `@paperclipai/adapter-utils`; the interface is structural, so hosts type-check against it unchanged.

## License

MIT
