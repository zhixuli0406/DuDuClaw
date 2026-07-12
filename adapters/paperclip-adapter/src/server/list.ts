/**
 * List DuDuClaw agents via `duduclaw agent list --json` (one JSON array on
 * stdout; logs go to stderr).
 */

import { execFile } from "node:child_process";
import type { DuduclawAgent } from "../types.js";

export interface ListAgentsOptions {
  binaryPath?: string;
  duduclawHome?: string;
  timeoutMs?: number;
}

export function listAgents(opts: ListAgentsOptions = {}): Promise<DuduclawAgent[]> {
  const binaryPath = opts.binaryPath ?? "duduclaw";
  return new Promise((resolve, reject) => {
    execFile(
      binaryPath,
      ["agent", "list", "--json"],
      {
        timeout: opts.timeoutMs ?? 15_000,
        env: opts.duduclawHome
          ? { ...process.env, DUDUCLAW_HOME: opts.duduclawHome }
          : process.env,
        maxBuffer: 4 * 1024 * 1024,
      },
      (err, stdout) => {
        if (err) {
          reject(new Error(`duduclaw agent list --json failed: ${err.message}`));
          return;
        }
        try {
          const parsed: unknown = JSON.parse(stdout.trim());
          if (!Array.isArray(parsed)) {
            reject(new Error("unexpected output: expected a JSON array"));
            return;
          }
          resolve(parsed as DuduclawAgent[]);
        } catch (parseErr) {
          reject(
            new Error(
              `could not parse agent list output: ${
                parseErr instanceof Error ? parseErr.message : String(parseErr)
              }`,
            ),
          );
        }
      },
    );
  });
}
