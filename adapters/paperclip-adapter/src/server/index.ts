import type { ServerAdapterModule } from "../types.js";
import { execute } from "./execute.js";
import { testEnvironment } from "./test.js";

export { execute } from "./execute.js";
export { testEnvironment } from "./test.js";
export { listAgents } from "./list.js";
export { McpClient } from "./mcp.js";

export function createServerAdapter(): ServerAdapterModule {
  return {
    type: "duduclaw",
    execute,
    testEnvironment,
  };
}
