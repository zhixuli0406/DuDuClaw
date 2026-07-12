/**
 * @duduclaw/paperclip-adapter — root metadata module.
 *
 * Per the paperclip external-adapter convention this module supplies the
 * adapter identity (`type` / `label` / `models` / configuration doc) and
 * re-exports `createServerAdapter` from the server entry.
 */

export const type = "duduclaw";
export const label = "DuDuClaw";

/**
 * DuDuClaw routes models per agent (`agent.toml [model] preferred`, OAuth
 * account rotation, local inference fallback); the adapter therefore does not
 * pin a model list — model choice lives on the DuDuClaw side.
 */
export const models: { id: string; label: string }[] = [];

export const agentConfigurationDoc = `# DuDuClaw adapter configuration

Runs a DuDuClaw agent through the local \`duduclaw\` binary.

| key | required | default | meaning |
| --- | --- | --- | --- |
| \`agentId\` | yes | — | DuDuClaw agent id (see \`duduclaw agent list --json\`) |
| \`binaryPath\` | no | \`duduclaw\` | absolute path to the duduclaw binary |
| \`duduclawHome\` | no | \`~/.duduclaw\` | overrides \`DUDUCLAW_HOME\` |
| \`promptTemplate\` | no | — | \`{{variable}}\` template rendered against the run context |
| \`timeoutMs\` | no | \`300000\` | max wait for the agent response |
| \`pollIntervalMs\` | no | \`2000\` | response poll interval |

A running DuDuClaw gateway (\`duduclaw run\` or \`duduclaw service start\`)
is required: the adapter enqueues work over the DuDuClaw MCP server and the
gateway dispatcher executes it.
`;

export type {
  AdapterExecutionContext,
  AdapterExecutionResult,
  AdapterEnvironmentTestContext,
  AdapterEnvironmentTestResult,
  ServerAdapterModule,
  DuduclawAgent,
} from "./types.js";

export { createServerAdapter, listAgents } from "./server/index.js";
