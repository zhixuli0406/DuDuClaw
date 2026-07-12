/**
 * Core execution: dispatch one task to a DuDuClaw agent and wait for its
 * response.
 *
 * Flow (all entry points verified against the DuDuClaw CLI source):
 * 1. `duduclaw mcp-server` is spawned and driven over stdio JSON-RPC.
 * 2. `send_to_agent { agent_id, prompt }` enqueues the task; the result
 *    carries a `_receipt.message_id`.
 * 3. The running DuDuClaw gateway (`duduclaw run` or the system service)
 *    consumes the queue and executes the agent — this adapter does not run
 *    models itself.
 * 4. `check_responses { agent_id }` is polled until a response newer than the
 *    dispatch appears (responses are previewed at 500 chars by the upstream
 *    tool; see README for the limitation).
 */

import type { AdapterExecutionContext, AdapterExecutionResult } from "../types.js";
import { McpClient } from "./mcp.js";

export function asString(v: unknown): string | undefined {
  return typeof v === "string" && v.trim().length > 0 ? v.trim() : undefined;
}

export function asNumber(v: unknown): number | undefined {
  return typeof v === "number" && Number.isFinite(v) ? v : undefined;
}

/** `{{variable}}` substitution for `config.promptTemplate`. */
export function renderTemplate(template: string, data: Record<string, unknown>): string {
  return template.replace(/\{\{\s*([\w.]+)\s*\}\}/g, (_m, key: string) => {
    const value = key
      .split(".")
      .reduce<unknown>(
        (acc, part) =>
          acc && typeof acc === "object" ? (acc as Record<string, unknown>)[part] : undefined,
        data,
      );
    return value === undefined || value === null ? "" : String(value);
  });
}

/**
 * Resolve the task prompt: `config.promptTemplate` rendered against the
 * execution context when present, otherwise the first non-empty of
 * `context.prompt` / `context.taskDescription` / `context.taskTitle`.
 */
export function resolvePrompt(
  config: Record<string, unknown>,
  context: Record<string, unknown>,
): string | undefined {
  const template = asString(config.promptTemplate);
  if (template) {
    const rendered = renderTemplate(template, context).trim();
    if (rendered.length > 0) return rendered;
  }
  return (
    asString(context.prompt) ??
    asString(context.taskDescription) ??
    asString(context.taskTitle)
  );
}

function fail(message: string): AdapterExecutionResult {
  return { exitCode: 1, signal: null, timedOut: false, errorMessage: message };
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

export async function execute(ctx: AdapterExecutionContext): Promise<AdapterExecutionResult> {
  const config = ctx.config ?? {};
  const binaryPath = asString(config.binaryPath) ?? "duduclaw";
  const agentId = asString(config.agentId);
  if (!agentId) {
    return fail(
      "config.agentId is required (the DuDuClaw agent id, e.g. from `duduclaw agent list --json`)",
    );
  }
  const timeoutMs = asNumber(config.timeoutMs) ?? 300_000;
  const pollIntervalMs = asNumber(config.pollIntervalMs) ?? 2_000;
  const prompt = resolvePrompt(config, ctx.context ?? {});
  if (!prompt) {
    return fail(
      "no task prompt found (expected context.prompt / context.taskDescription / context.taskTitle, or set config.promptTemplate)",
    );
  }

  const client = new McpClient({
    binaryPath,
    onLog: ctx.onLog,
    env: asString(config.duduclawHome) ? { DUDUCLAW_HOME: asString(config.duduclawHome)! } : undefined,
  });
  const startedAt = new Date().toISOString();
  if (client.pid !== undefined) {
    await ctx.onSpawn?.({ pid: client.pid, startedAt });
  }

  try {
    await client.initialize(asNumber(config.initTimeoutMs) ?? 10_000);

    const dispatch = await client.callTool("send_to_agent", {
      agent_id: agentId,
      prompt,
    });
    if (dispatch.isError) {
      return fail(`send_to_agent failed: ${dispatch.text}`);
    }
    const messageId =
      (dispatch.receipt && asString(dispatch.receipt.message_id)) ??
      /message_id=([0-9a-fA-F-]+)/.exec(dispatch.text)?.[1];
    await ctx.onLog(
      "stdout",
      `[duduclaw] dispatched to agent '${agentId}'` +
        (messageId ? ` (message_id=${messageId})` : "") +
        "\n",
    );

    // Poll for a response newer than the dispatch. `check_responses` reports
    // the newest responses first; an entry timestamped after `startedAt` is
    // ours (the queue is per-target-agent).
    const deadline = Date.now() + timeoutMs;
    while (Date.now() < deadline) {
      await sleep(pollIntervalMs);
      const res = await client.callTool("check_responses", {
        agent_id: agentId,
        limit: 3,
      });
      if (res.isError) continue; // transient — keep polling until deadline
      const responseText = extractFreshResponse(res.text, startedAt);
      if (responseText !== undefined) {
        await ctx.onLog("stdout", responseText + "\n");
        return {
          exitCode: 0,
          signal: null,
          timedOut: false,
          provider: "duduclaw",
          sessionParams: messageId ? { lastMessageId: messageId } : null,
        };
      }
    }
    return {
      exitCode: null,
      signal: null,
      timedOut: true,
      errorMessage:
        `no response from agent '${agentId}' within ${timeoutMs}ms — ` +
        "make sure the DuDuClaw gateway is running (`duduclaw run` or `duduclaw service start`)",
    };
  } catch (err) {
    return fail(err instanceof Error ? err.message : String(err));
  } finally {
    client.close();
  }
}

/**
 * Parse the human-readable `check_responses` output and return the full text
 * when it contains a response whose RFC 3339 timestamp is >= `sinceIso`.
 *
 * Upstream format (duduclaw `handle_check_responses`):
 *
 *     Found N response(s) from agent '<id>':
 *
 *     --- Response 1 (<rfc3339 ts>)[ [truncated, full=X chars]] ---
 *     <up to 500 chars of preview>
 *
 * Parsed defensively — an unrecognized format returns undefined (keep
 * polling) rather than a false positive.
 */
export function extractFreshResponse(text: string, sinceIso: string): string | undefined {
  if (!text || /No responses found/i.test(text)) return undefined;
  const since = Date.parse(sinceIso);
  if (Number.isNaN(since)) return undefined;
  const matches = [...text.matchAll(/^--- Response \d+ \(([^)]+)\)/gm)];
  for (const m of matches) {
    const ts = Date.parse(m[1] ?? "");
    if (!Number.isNaN(ts) && ts >= since) {
      return text;
    }
  }
  return undefined;
}
