/**
 * Environment diagnostics: verifies the duduclaw binary is reachable and
 * lists how many agents are available. Never throws — failures come back as
 * structured checks.
 */

import { execFile } from "node:child_process";
import type {
  AdapterEnvironmentCheck,
  AdapterEnvironmentTestContext,
  AdapterEnvironmentTestResult,
} from "../types.js";
import { listAgents } from "./list.js";

function version(binaryPath: string, timeoutMs: number): Promise<string> {
  return new Promise((resolve, reject) => {
    execFile(binaryPath, ["--version"], { timeout: timeoutMs }, (err, stdout) => {
      if (err) reject(err);
      else resolve(stdout.trim());
    });
  });
}

export async function testEnvironment(
  ctx: AdapterEnvironmentTestContext,
): Promise<AdapterEnvironmentTestResult> {
  const config = ctx.config ?? {};
  const binaryPath =
    typeof config.binaryPath === "string" && config.binaryPath.trim().length > 0
      ? config.binaryPath.trim()
      : "duduclaw";
  const duduclawHome =
    typeof config.duduclawHome === "string" && config.duduclawHome.trim().length > 0
      ? config.duduclawHome.trim()
      : undefined;
  const checks: AdapterEnvironmentCheck[] = [];

  try {
    const v = await version(binaryPath, 10_000);
    checks.push({
      level: "info",
      message: `duduclaw binary found: ${v}`,
      code: "binary_ok",
    });
  } catch (err) {
    checks.push({
      level: "error",
      message: `duduclaw binary not found or not runnable at '${binaryPath}': ${
        err instanceof Error ? err.message : String(err)
      }`,
      hint: "install DuDuClaw and/or set config.binaryPath to the absolute binary path",
      code: "binary_missing",
    });
    return {
      adapterType: ctx.adapterType,
      status: "fail",
      checks,
      testedAt: new Date().toISOString(),
    };
  }

  try {
    const agents = await listAgents({ binaryPath, duduclawHome });
    if (agents.length === 0) {
      checks.push({
        level: "warn",
        message: "no DuDuClaw agents registered yet",
        hint: "run `duduclaw onboard` or `duduclaw agent create <name>` first",
        code: "no_agents",
      });
    } else {
      checks.push({
        level: "info",
        message: `${agents.length} DuDuClaw agent(s) available: ${agents
          .map((a) => a.name)
          .join(", ")}`,
        code: "agents_ok",
      });
    }
  } catch (err) {
    checks.push({
      level: "warn",
      message: `could not list agents: ${err instanceof Error ? err.message : String(err)}`,
      hint: "requires duduclaw >= 1.36 (agent list --json)",
      code: "list_failed",
    });
  }

  checks.push({
    level: "info",
    message:
      "task execution requires a running DuDuClaw gateway (`duduclaw run` or `duduclaw service start`)",
    code: "gateway_note",
  });

  const status = checks.some((c) => c.level === "error")
    ? "fail"
    : checks.some((c) => c.level === "warn")
      ? "warn"
      : "pass";
  return {
    adapterType: ctx.adapterType,
    status,
    checks,
    testedAt: new Date().toISOString(),
  };
}
