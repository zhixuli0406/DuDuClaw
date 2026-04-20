# Agent Client Protocol (ACP/A2A)

> Let the IDE talk to the agent the same way the agent talks to its tools — stdio JSON-RPC 2.0, discoverable, language-agnostic.

---

## The Metaphor: A Restaurant's Reservation Line

Every restaurant has two entrances:

- **The dining room** — where customers walk in, browse the menu, and interact with servers.
- **The reservation line** — a telephone protocol. A reservation system on the other end doesn't need to know what the dining room looks like; it just needs to ask *"do you have a table for four at 7 PM?"* and get a yes/no.

IDEs like Zed, JetBrains, and Neovim want to ask an agent *"can you take this task?"* without having to understand DuDuClaw's entire channel infrastructure. They need the reservation line — a clean, stable, protocol-driven interface.

That's what the ACP server is.

---

## What Is ACP/A2A?

**ACP** stands for Agent Client Protocol — a stdio JSON-RPC 2.0 line protocol for IDE ↔ agent communication. **A2A** is the related "Agent to Agent" protocol for agent discovery and task exchange.

Together they let an IDE (or another agent, or a CI pipeline, or a shell script) treat a running DuDuClaw agent as a first-class service — one it can discover, send tasks to, poll for status, and cancel.

DuDuClaw ships the server side:

```
duduclaw acp-server
     |
     v
Listens on stdin, writes to stdout (line-delimited JSON-RPC 2.0)
     |
     v
Responds to:
  agent/discover   → return AgentCard
  tasks/send       → queue new task
  tasks/get        → poll task status
  tasks/cancel     → cancel running task
```

Before v1.8.9, `duduclaw acp-server` was a placeholder that printed a message and returned. v1.8.9 wired it to a real `A2ATaskManager` and made it functional.

---

## The Agent Card

Every ACP server can describe itself. When a client connects, it can issue `agent/discover` and receive an **Agent Card** — a JSON document with identity, capabilities, and skills:

```json
{
  "name": "duduclaw-pm",
  "description": "Project manager for DuDuClaw v1.9 roadmap",
  "url": "stdio://duduclaw acp-server --agent duduclaw-pm",
  "version": "1.8.14",
  "capabilities": {
    "streaming": true,
    "multi_turn": true,
    "tool_use": true
  },
  "skills": [
    {
      "name": "task_planning",
      "description": "Break down features into TaskSpec workflows",
      "tags": ["planning", "orchestration"]
    },
    {
      "name": "sprint_review",
      "description": "Summarize sprint outcomes from memory + tasks",
      "tags": ["reporting", "retrospective"]
    }
  ]
}
```

This is the same document format as A2A's `.well-known/agent.json` discovery endpoint. An IDE can cache it, display available skills in a UI, and decide whether to route a request to this particular agent.

### `.well-known` Generation

For agents exposed over HTTP (a future extension), DuDuClaw can emit a `.well-known/agent.json` file so external clients can discover the agent without first connecting:

```
/etc/duduclaw/agents/dudu/.well-known/agent.json
     ↓
https://your-host.example.com/.well-known/agent.json
```

Any A2A-compatible client reads it, learns the agent's capabilities, and decides whether to proceed.

---

## The JSON-RPC Loop

The stdio server is a simple line-delimited JSON-RPC 2.0 loop:

```
loop {
    read line from stdin
    parse JSON-RPC 2.0 request
    dispatch to handler:
        agent/discover  → return AgentCard
        tasks/send      → TaskManager.send(params)
        tasks/get       → TaskManager.get(id)
        tasks/cancel    → TaskManager.cancel(id)
    write JSON-RPC 2.0 response to stdout
}
```

JSON-RPC over stdio is the same transport MCP uses — if you've already built an MCP server, you've already built 95% of an ACP server.

### Example Session

```
→ {"jsonrpc":"2.0","id":1,"method":"agent/discover"}
← {"jsonrpc":"2.0","id":1,"result":{
     "name":"duduclaw-pm",
     "version":"1.8.14",
     "capabilities":{"streaming":true,"multi_turn":true,"tool_use":true},
     "skills":[...]
   }}

→ {"jsonrpc":"2.0","id":2,"method":"tasks/send","params":{
     "task":"Draft the v1.9 release notes from the last 50 commits",
     "priority":"high"
   }}
← {"jsonrpc":"2.0","id":2,"result":{
     "task_id":"t_abc123",
     "status":"queued"
   }}

→ {"jsonrpc":"2.0","id":3,"method":"tasks/get","params":{"task_id":"t_abc123"}}
← {"jsonrpc":"2.0","id":3,"result":{
     "task_id":"t_abc123",
     "status":"completed",
     "output":"# DuDuClaw v1.9 Release Notes\n\n..."
   }}
```

---

## The A2ATaskManager

Behind `tasks/send`, `tasks/get`, and `tasks/cancel` lives the `A2ATaskManager`. Its job is to:

1. **Queue** incoming tasks into the agent's existing task system (`TaskSpec`, `tasks/` directory).
2. **Track** status transitions (queued → running → completed/failed/cancelled).
3. **Route** task execution through the agent's normal runtime (Claude / Codex / Gemini / OpenAI-compat).
4. **Expose** results in the task envelope so the client can poll for them.

This means tasks submitted via ACP flow through the **same** pipelines as tasks submitted via channels or MCP tools — single source of truth, unified observability in the Logs/Activity dashboard.

---

## Why IDE Integration Matters

### Zed

[Zed](https://zed.dev) exposes an "agent panel" that can talk to any ACP-compatible agent. Point it at `duduclaw acp-server --agent <your-agent>` and Zed gets native access to:
- Task routing (via `tasks/send`)
- Inline responses in the editor
- Multi-turn follow-up within the IDE

### JetBrains

The IntelliJ platform's AI Assistant can be extended to speak ACP via a plugin. Once connected, the agent can browse the project, suggest refactors that flow through the dispatcher, and land commits through the worktree isolation layer.

### Neovim

`nvim-acp` plugins use the stdio line protocol directly — `duduclaw acp-server` is a drop-in backend. You get command-line-driven agent access without leaving the editor.

### CI/CD Pipelines

A pipeline step can send a task via ACP and poll for completion:

```yaml
- name: Generate release notes via DuDuClaw
  run: |
    echo '{"jsonrpc":"2.0","id":1,"method":"tasks/send","params":{"task":"..."}}' \
      | duduclaw acp-server --agent duduclaw-pm
```

No HTTP server, no auth tokens, no port management — just stdio in the container.

---

## The Three Stdio Protocols DuDuClaw Speaks

This is worth mapping because the naming overlaps:

| Protocol | Purpose | Direction | Command |
|----------|---------|-----------|---------|
| **MCP** | Expose DuDuClaw's tools (channel, memory, agent, wiki, task, ...) to an AI runtime | Runtime → DuDuClaw | `duduclaw mcp-server` |
| **ACP/A2A** | Let external clients (IDEs, pipelines, other agents) send tasks to DuDuClaw | IDE → DuDuClaw | `duduclaw acp-server` |
| **Runtime stdio** | DuDuClaw spawns a runtime (Claude/Codex/Gemini) subprocess and talks to it via stdio JSON | DuDuClaw → Runtime | *Internal* |

They're three distinct conversations, all on stdio, all JSON-RPC-adjacent. The same agent participates in all three simultaneously at runtime.

---

## Why Stdio, Not HTTP?

Stdio has a few practical advantages for IDE integration:

- **Zero configuration** — no port to pick, no TLS cert, no firewall rule.
- **Process-scoped** — the ACP server lives and dies with the IDE session. No orphaned listeners.
- **OS-level auth** — if you can spawn the process, you already have the permission you need. No API keys.
- **Transport-agnostic** — the same line protocol can be tunneled over SSH, inside a container, or across a VS Code remote session.

HTTP is still available for the Dashboard and Prometheus metrics, but for IDE ↔ agent, stdio is simpler and safer.

---

## Streaming & Multi-Turn

The Agent Card advertises `streaming: true` and `multi_turn: true`. This signals to the client that:

- **Streaming**: long-running tasks can emit progress events over the same stdio connection, not just a single response.
- **Multi-turn**: a task context can span multiple request/response pairs (clarifications, follow-ups) without losing state.

These capabilities mirror the Session Memory Stack — pinned instructions, snowball recap, and key facts all carry across multi-turn ACP conversations the same way they do in channel messages.

---

## Security Considerations

ACP, like MCP, inherits DuDuClaw's security boundaries:

- **CONTRACT.toml** — must_not/must_always rules still apply; an ACP-submitted task can't violate them.
- **Capability gating** — `agent.toml [capabilities]` deny-by-default still gates tool access.
- **Audit log** — tasks submitted via ACP appear in `audit.unified_log` with source=`acp`.
- **Sandboxing** — tasks still run through the worktree layer and (optionally) container sandbox.

The client being an IDE doesn't grant elevated trust — the agent's own policies are the last line of defense.

---

## Interaction with Other Systems

- **Task Board**: ACP-submitted tasks flow through the same `TaskStore` as channel-submitted ones. Both show in the Dashboard Activity Feed.
- **Runtime selection**: The agent's normal runtime (Claude/Codex/Gemini/OpenAI) handles ACP tasks — same session memory, same prompt cache strategy, same account rotation.
- **Evolution**: ACP tasks count as "substantive turns" for Key-Fact extraction and prediction error calibration.
- **Audit log**: All ACP requests are logged with source=`acp`, alongside the other four audit sources (security / tool_calls / channel_failures / feedback).

---

## Why This Matters

### Standards-Based Integration

ACP is a real protocol with real clients (Zed, nvim-acp, experimental JetBrains plugins). Supporting it puts DuDuClaw in a growing ecosystem instead of requiring custom integrations per IDE.

### Same Agent, New Interface

No new agents, no new configuration, no new runtime boundaries. The existing agent (SOUL.md, CONTRACT.toml, memory, skills, wiki) is simply reachable from a new entry point. All the investment in agent behavior transfers.

### Developer Loop Acceleration

Instead of asking an agent a question in a chat app and then copy-pasting the response into the editor, developers can invoke the agent directly from where they work. The friction drops to near-zero, and the agent's responses land *in context*.

### Composable Orchestration

One agent can be an A2A client to another agent. Orchestrator-style agents can discover sub-agents via `agent/discover`, check their skill tags, and route tasks via `tasks/send` — a structured, standard alternative to DuDuClaw's internal file-based IPC for cross-process scenarios.

---

## The Takeaway

A good agent should be reachable from wherever work happens. The dining room (channels) is for end users; the reservation line (ACP/A2A) is for the IDEs, pipelines, and peer agents that need to work with it programmatically. Same agent, same brain, same contracts — just a cleaner protocol on the front door.
