#!/usr/bin/env node
/**
 * Mock duduclaw binary for adapter tests. Emulates the three entry points the
 * adapter uses: `--version`, `agent list --json`, and `mcp-server` (stdio
 * JSON-RPC, newline-delimited — same framing as the real server).
 */

const argv = process.argv.slice(2);
const cmd = argv[0];

if (cmd === "--version") {
  console.log("duduclaw 1.35.0-mock");
  process.exit(0);
}

if (cmd === "agent" && argv[1] === "list" && argv.includes("--json")) {
  console.error("mock: listing agents (stderr noise must not break parsing)");
  console.log(
    JSON.stringify([
      {
        name: "boss",
        display_name: "Boss",
        role: "main",
        status: "active",
        trigger: "@Boss",
        reports_to: "",
        icon: "🐾",
        model: "claude-sonnet-4-6",
      },
      {
        name: "worker",
        display_name: "Worker",
        role: "specialist",
        status: "active",
        trigger: "@Worker",
        reports_to: "boss",
        icon: "🤖",
        model: "claude-sonnet-4-6",
      },
    ]),
  );
  process.exit(0);
}

if (cmd === "mcp-server") {
  let checkCalls = 0;
  let buf = "";
  process.stdin.setEncoding("utf8");
  process.stdin.on("data", (chunk) => {
    buf += chunk;
    let idx;
    while ((idx = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, idx).trim();
      buf = buf.slice(idx + 1);
      if (!line) continue;
      let msg;
      try {
        msg = JSON.parse(line);
      } catch {
        continue;
      }
      const reply = (result) =>
        process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }) + "\n");
      if (msg.method === "initialize") {
        reply({
          protocolVersion: "2024-11-05",
          capabilities: { tools: {} },
          serverInfo: { name: "duduclaw-mock", version: "0.0.0" },
        });
        continue;
      }
      if (msg.method === "tools/call") {
        const name = msg.params && msg.params.name;
        const args = (msg.params && msg.params.arguments) || {};
        if (name === "send_to_agent") {
          if (!args.agent_id || !args.prompt) {
            reply({
              content: [{ type: "text", text: "Error: agent_id and prompt are required" }],
              isError: true,
            });
            continue;
          }
          reply({
            content: [
              {
                type: "text",
                text: `Receipt: message_id=mock-msg-1, target=${args.agent_id}, status=queued.`,
              },
            ],
            _receipt: { message_id: "mock-msg-1", target: args.agent_id, status: "queued" },
          });
        } else if (name === "check_responses") {
          checkCalls += 1;
          if (checkCalls < 2) {
            reply({
              content: [
                { type: "text", text: `No responses found from agent '${args.agent_id}'.` },
              ],
            });
          } else {
            reply({
              content: [
                {
                  type: "text",
                  text:
                    `Found 1 response(s) from agent '${args.agent_id}':\n\n` +
                    `--- Response 1 (${new Date().toISOString()}) ---\n` +
                    "Hello from the mock agent\n",
                },
              ],
            });
          }
        } else {
          reply({
            content: [{ type: "text", text: `Error: unknown tool ${name}` }],
            isError: true,
          });
        }
        continue;
      }
      process.stdout.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: msg.id === undefined ? null : msg.id,
          error: { code: -32601, message: `Method not found: ${msg.method}` },
        }) + "\n",
      );
    }
  });
  process.stdin.on("end", () => process.exit(0));
} else if (cmd !== "--version") {
  console.error(`mock duduclaw: unknown command: ${argv.join(" ")}`);
  process.exit(2);
}
