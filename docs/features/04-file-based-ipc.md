# File-Based IPC Message Bus

> Zero-dependency inter-agent communication via append-only JSONL.

---

## The Metaphor: The Office Bulletin Board

Most multi-agent systems require you to install and maintain a message broker — Redis, RabbitMQ, Kafka. That's like requiring every small office to have a corporate email server, an IT team, and an SLA.

DuDuClaw takes a different approach: **the bulletin board**.

Imagine a shared corkboard in the office hallway:
- Agent A pins a task note on the board (appends a line to a file)
- The office manager walks by periodically, reads new notes (the dispatcher polls the file)
- The office manager hands the note to Agent B (spawns a subprocess)
- Agent B pins the result back on the board

No email server. No IT team. No SLA. Just a corkboard, some pushpins, and a reliable office manager.

---

## How It Works

### The Message Format

Each message is a single JSON line appended to `bus_queue.jsonl`:

```
{"from": "agent-a", "to": "agent-b", "task": "summarize-report", "payload": {...}, "ts": "2026-04-07T10:30:00Z"}
{"from": "agent-b", "to": "agent-a", "task": "summary-result", "payload": {...}, "ts": "2026-04-07T10:30:15Z"}
```

One line = one message. The file grows over time like an append-only log.

### The Dispatch Cycle

```
HeartbeatScheduler fires (periodic interval)
     |
     v
AgentDispatcher reads bus_queue.jsonl
     |
     v
Any new messages for agents I manage?
     |
  +--+--+
  |     |
 Yes    No
  |     |
  |     v
  |   Sleep until next heartbeat
  |
  v
For each pending message:
  |
  v
Check max_concurrent_runs semaphore
  |
  +---> Slots available? --> Spawn Claude CLI subprocess with task
  |
  +---> All slots full? --> Leave message for next cycle
```

### Why JSONL?

JSONL (JSON Lines) has properties that make it ideal for this use case:

**Append-only is naturally concurrent-safe.** Multiple processes can append to the same file without corrupting each other's data. Each `append` operation writes a complete line — there's no interleaving risk.

**Files are inherently persistent.** If the system crashes mid-operation, everything that was written before the crash is preserved. No message loss, no recovery protocol needed.

**Human-readable.** You can debug the entire message history with `tail -f bus_queue.jsonl`. No special tools, no admin console, no protocol decoder.

**Zero dependencies.** No broker to install, configure, monitor, patch, or troubleshoot. The filesystem is the broker.

### The Concurrency Semaphore

Each agent has a `max_concurrent_runs` setting in its heartbeat configuration. This prevents a flood of messages from overwhelming the system:

```
Agent "agnes" config:
  max_concurrent_runs = 3

Current state:
  Running: 2 tasks
  Pending: 5 messages

Dispatcher decision:
  - Start 1 more task (reaching limit of 3)
  - Hold remaining 4 messages for next cycle
```

This is a simple counting semaphore — no complex queue management, no priority scheduling. When a task completes, its slot opens up for the next pending message.

---

## The HeartbeatScheduler

The dispatcher doesn't run continuously — it's driven by the HeartbeatScheduler, which provides a unified timing mechanism for each agent:

```
HeartbeatScheduler (per agent)
     |
     +---> Poll bus_queue.jsonl for new tasks
     |
     +---> Check GVU silence breaker
     |         (if agent hasn't evolved recently,
     |          trigger a proactive reflection)
     |
     +---> Fire any due cron tasks
```

The interval is configurable per agent. A high-traffic customer support agent might poll every 5 seconds; a background analytics agent might poll every 5 minutes.

---

## Why This Matters

### Operational Simplicity

Every external dependency is a potential point of failure. Redis needs memory management, monitoring, and backup. Kafka needs ZooKeeper (or KRaft), topic management, and consumer group coordination. A JSONL file needs... a filesystem.

For a system designed to run as a single binary on a developer's machine, this simplicity is a feature, not a compromise.

### Natural Durability

Messages written to the file survive process restarts, system reboots, and crashes. There's no "in-flight" state that can be lost — once a line is appended, it's permanent.

### Debugging

When something goes wrong in a message broker, you're debugging opaque internal state. When something goes wrong with a JSONL file, you open it in a text editor. The entire history of every inter-agent communication is right there, in chronological order, in plain text.

### Scalability Boundary

This approach has a natural ceiling: it works well for tens of agents exchanging hundreds of messages per day. It would not scale to millions of messages per second. But that's exactly the right trade-off for DuDuClaw's use case — individual developers or small teams running a handful of specialized agents.

---

## Interaction with Other Systems

- **HeartbeatScheduler**: Drives the polling rhythm for each agent.
- **Agent Registry**: The dispatcher knows which agents exist and their concurrency limits.
- **Container Sandbox**: When a task requires isolation, the dispatcher spawns the subprocess inside a container instead of directly on the host.
- **Audit Log**: All dispatched tasks are recorded in the JSONL audit trail.

---

## The Takeaway

The simplest solution that works is usually the best solution. For inter-agent communication at DuDuClaw's scale, a JSONL file provides everything a message broker would — persistence, concurrency safety, observability — without any operational overhead.
