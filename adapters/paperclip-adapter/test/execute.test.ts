import { chmodSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { beforeAll, describe, expect, it } from "vitest";

import { createServerAdapter } from "../src/server/index.js";
import {
  execute,
  extractFreshResponse,
  renderTemplate,
  resolvePrompt,
} from "../src/server/execute.js";
import { listAgents } from "../src/server/list.js";
import { testEnvironment } from "../src/server/test.js";
import type { AdapterExecutionContext } from "../src/types.js";

const FAKE_BIN = fileURLToPath(new URL("./fixtures/fake-duduclaw.mjs", import.meta.url));

beforeAll(() => {
  chmodSync(FAKE_BIN, 0o755);
});

function makeCtx(
  config: Record<string, unknown>,
  context: Record<string, unknown>,
): AdapterExecutionContext & { logs: { stream: string; chunk: string }[] } {
  const logs: { stream: string; chunk: string }[] = [];
  return {
    runId: "run-1",
    agent: { id: "a-1", companyId: "c-1", name: "Boss", adapterConfig: config },
    runtime: { sessionId: null, sessionParams: null },
    config,
    context,
    logs,
    onLog: async (stream, chunk) => {
      logs.push({ stream, chunk });
    },
  };
}

describe("renderTemplate", () => {
  it("substitutes {{vars}} with dotted paths and drops unknowns", () => {
    expect(renderTemplate("do {{taskTitle}} for {{a.b}} ({{missing}})", {
      taskTitle: "the thing",
      a: { b: "me" },
    })).toBe("do the thing for me ()");
  });
});

describe("resolvePrompt", () => {
  it("prefers promptTemplate, then context fields", () => {
    expect(
      resolvePrompt({ promptTemplate: "T: {{taskTitle}}" }, { taskTitle: "x" }),
    ).toBe("T: x");
    expect(resolvePrompt({}, { prompt: "direct" })).toBe("direct");
    expect(resolvePrompt({}, { taskTitle: "title only" })).toBe("title only");
    expect(resolvePrompt({}, {})).toBeUndefined();
  });
});

describe("extractFreshResponse", () => {
  it("matches only responses newer than the dispatch time", () => {
    const old = "Found 1 response(s) from agent 'a':\n\n--- Response 1 (2020-01-01T00:00:00Z) ---\nstale\n";
    expect(extractFreshResponse(old, new Date().toISOString())).toBeUndefined();
    const fresh = `Found 1 response(s) from agent 'a':\n\n--- Response 1 (${new Date().toISOString()}) ---\nnew\n`;
    expect(extractFreshResponse(fresh, new Date(Date.now() - 60_000).toISOString())).toContain(
      "new",
    );
    expect(extractFreshResponse("No responses found from agent 'a'.", "2020-01-01T00:00:00Z")).toBeUndefined();
    expect(extractFreshResponse("garbage output", "2020-01-01T00:00:00Z")).toBeUndefined();
  });
});

describe("createServerAdapter", () => {
  it("exposes the duduclaw adapter type and required functions", () => {
    const adapter = createServerAdapter();
    expect(adapter.type).toBe("duduclaw");
    expect(typeof adapter.execute).toBe("function");
    expect(typeof adapter.testEnvironment).toBe("function");
  });
});

describe("execute (mocked binary)", () => {
  it("fails closed without config.agentId", async () => {
    const ctx = makeCtx({ binaryPath: FAKE_BIN }, { prompt: "hi" });
    const res = await execute(ctx);
    expect(res.exitCode).toBe(1);
    expect(res.errorMessage).toMatch(/agentId/);
  });

  it("fails closed without a prompt", async () => {
    const ctx = makeCtx({ binaryPath: FAKE_BIN, agentId: "boss" }, {});
    const res = await execute(ctx);
    expect(res.exitCode).toBe(1);
    expect(res.errorMessage).toMatch(/prompt/);
  });

  it("dispatches and returns the agent response", async () => {
    const ctx = makeCtx(
      { binaryPath: FAKE_BIN, agentId: "boss", timeoutMs: 10_000, pollIntervalMs: 25 },
      { taskTitle: "summarize the sprint" },
    );
    const res = await execute(ctx);
    expect(res.errorMessage ?? null).toBeNull();
    expect(res.exitCode).toBe(0);
    expect(res.timedOut).toBe(false);
    expect(res.sessionParams).toEqual({ lastMessageId: "mock-msg-1" });
    const stdout = ctx.logs
      .filter((l) => l.stream === "stdout")
      .map((l) => l.chunk)
      .join("");
    expect(stdout).toContain("dispatched to agent 'boss'");
    expect(stdout).toContain("Hello from the mock agent");
  });

  it("reports a spawn failure honestly", async () => {
    const ctx = makeCtx(
      { binaryPath: "/nonexistent/duduclaw-bin", agentId: "boss", timeoutMs: 2_000 },
      { prompt: "hi" },
    );
    const res = await execute(ctx);
    expect(res.exitCode).toBe(1);
    expect(res.errorMessage).toBeTruthy();
  });
});

describe("listAgents (mocked binary)", () => {
  it("parses the JSON array and ignores stderr noise", async () => {
    const agents = await listAgents({ binaryPath: FAKE_BIN });
    expect(agents.map((a) => a.name)).toEqual(["boss", "worker"]);
    expect(agents[1]?.reports_to).toBe("boss");
  });
});

describe("testEnvironment (mocked binary)", () => {
  it("passes with the mock binary present", async () => {
    const res = await testEnvironment({
      adapterType: "duduclaw",
      config: { binaryPath: FAKE_BIN },
    });
    expect(res.adapterType).toBe("duduclaw");
    expect(res.status).not.toBe("fail");
    expect(res.checks.some((c) => c.code === "binary_ok")).toBe(true);
  });

  it("fails when the binary is missing", async () => {
    const res = await testEnvironment({
      adapterType: "duduclaw",
      config: { binaryPath: "/nonexistent/duduclaw-bin" },
    });
    expect(res.status).toBe("fail");
    expect(res.checks.some((c) => c.code === "binary_missing")).toBe(true);
  });
});
