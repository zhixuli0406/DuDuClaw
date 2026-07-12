/**
 * Minimal stdio JSON-RPC 2.0 client for `duduclaw mcp-server`.
 *
 * The DuDuClaw MCP server speaks newline-delimited JSON-RPC over
 * stdin/stdout (protocol version 2024-11-05). Tool results arrive as
 * `{ content: [{ type: "text", text }], isError?, _receipt? }`.
 */

import { spawn, type ChildProcessWithoutNullStreams } from "node:child_process";

export interface ToolCallResult {
  text: string;
  isError: boolean;
  /** Structured receipt some tools attach (e.g. send_to_agent). */
  receipt?: Record<string, unknown>;
}

interface Pending {
  resolve: (value: unknown) => void;
  reject: (err: Error) => void;
  timer: NodeJS.Timeout;
}

export interface McpClientOptions {
  binaryPath: string;
  args?: string[];
  env?: Record<string, string>;
  onLog?: (stream: "stdout" | "stderr", chunk: string) => void | Promise<void>;
}

export class McpClient {
  private proc: ChildProcessWithoutNullStreams;
  private nextId = 1;
  private pending = new Map<number, Pending>();
  private buffer = "";
  private exited = false;
  private exitError: Error | null = null;

  constructor(opts: McpClientOptions) {
    this.proc = spawn(opts.binaryPath, opts.args ?? ["mcp-server"], {
      stdio: ["pipe", "pipe", "pipe"],
      env: { ...process.env, ...opts.env },
    });
    this.proc.stdout.on("data", (chunk: Buffer) => {
      this.onData(chunk.toString("utf8"));
    });
    this.proc.stderr.on("data", (chunk: Buffer) => {
      void opts.onLog?.("stderr", chunk.toString("utf8"));
    });
    this.proc.on("error", (err) => {
      this.failAll(new Error(`duduclaw mcp-server spawn failed: ${err.message}`));
    });
    this.proc.on("exit", (code, signal) => {
      this.exited = true;
      this.failAll(
        new Error(
          `duduclaw mcp-server exited (code=${code ?? "null"}, signal=${signal ?? "null"})`,
        ),
      );
    });
  }

  get pid(): number | undefined {
    return this.proc.pid;
  }

  private onData(text: string): void {
    this.buffer += text;
    let idx: number;
    while ((idx = this.buffer.indexOf("\n")) >= 0) {
      const line = this.buffer.slice(0, idx).trim();
      this.buffer = this.buffer.slice(idx + 1);
      if (!line) continue;
      let msg: Record<string, unknown>;
      try {
        msg = JSON.parse(line) as Record<string, unknown>;
      } catch {
        continue; // non-JSON noise on stdout is ignored, never fatal
      }
      const id = typeof msg.id === "number" ? msg.id : undefined;
      if (id === undefined) continue;
      const entry = this.pending.get(id);
      if (!entry) continue;
      this.pending.delete(id);
      clearTimeout(entry.timer);
      if (msg.error && typeof msg.error === "object") {
        const err = msg.error as { message?: string; code?: number };
        entry.reject(
          new Error(`JSON-RPC error ${err.code ?? ""}: ${err.message ?? "unknown"}`),
        );
      } else {
        entry.resolve(msg.result);
      }
    }
  }

  private failAll(err: Error): void {
    this.exitError = err;
    for (const [, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(err);
    }
    this.pending.clear();
  }

  request(method: string, params: unknown, timeoutMs: number): Promise<unknown> {
    if (this.exited) {
      return Promise.reject(this.exitError ?? new Error("mcp-server already exited"));
    }
    const id = this.nextId++;
    const payload = JSON.stringify({ jsonrpc: "2.0", id, method, params }) + "\n";
    return new Promise<unknown>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`request '${method}' timed out after ${timeoutMs}ms`));
      }, timeoutMs);
      this.pending.set(id, { resolve, reject, timer });
      this.proc.stdin.write(payload, (err) => {
        if (err) {
          this.pending.delete(id);
          clearTimeout(timer);
          reject(err);
        }
      });
    });
  }

  async initialize(timeoutMs = 10_000): Promise<void> {
    await this.request(
      "initialize",
      {
        protocolVersion: "2024-11-05",
        clientInfo: { name: "@duduclaw/paperclip-adapter", version: "0.1.0" },
        capabilities: {},
      },
      timeoutMs,
    );
  }

  async callTool(
    name: string,
    args: Record<string, unknown>,
    timeoutMs = 30_000,
  ): Promise<ToolCallResult> {
    const result = (await this.request(
      "tools/call",
      { name, arguments: args },
      timeoutMs,
    )) as Record<string, unknown> | null;
    const content = Array.isArray(result?.content) ? result.content : [];
    const text = content
      .map((c: unknown) =>
        c && typeof c === "object" && "text" in c ? String((c as { text: unknown }).text) : "",
      )
      .filter((t: string) => t.length > 0)
      .join("\n");
    const receipt =
      result && typeof result._receipt === "object" && result._receipt !== null
        ? (result._receipt as Record<string, unknown>)
        : undefined;
    return { text, isError: result?.isError === true, receipt };
  }

  close(): void {
    try {
      this.proc.stdin.end();
    } catch {
      /* already closed */
    }
    if (!this.exited) {
      const proc = this.proc;
      proc.kill("SIGTERM");
      const killTimer = setTimeout(() => {
        if (!this.exited) proc.kill("SIGKILL");
      }, 3_000);
      killTimer.unref();
    }
  }
}
