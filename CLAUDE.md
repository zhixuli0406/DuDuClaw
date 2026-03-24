# DuDuClaw Project Guidelines

## Architecture Overview (v0.6.0)

DuDuClaw is a **Claude Code extension layer** — not a standalone AI platform. The AI brain is Claude Code SDK (`claude` CLI); DuDuClaw provides the plumbing: channel routing, session management, memory, evolution, and multi-account rotation.

Key architectural decisions:
- **MCP Server** (`duduclaw mcp-server`) exposes channel, memory, agent, and skill tools to Claude Code via JSON-RPC 2.0 over stdin/stdout
- **Agent directories** are Claude Code compatible: each contains `.claude/`, `SOUL.md`, `CLAUDE.md`, `.mcp.json`
- **Sub-agent orchestration** via `create_agent` / `spawn_agent` / `list_agents` MCP tools with `reports_to` hierarchy
- **Session Manager** persists conversations in SQLite with 50k token auto-compression (CJK-aware token estimation)
- **File-based IPC** (`bus_queue.jsonl`) for inter-agent delegation; **AgentDispatcher** consumes and spawns Claude CLI subprocesses
- **Container sandbox** (Docker / Apple Container) for agent task isolation with `--network=none`, tmpfs, read-only rootfs
- **Python subprocess** bridge for Claude Code SDK chat and evolution engine
- **Three channels**: Telegram (long polling), LINE (webhook), Discord (Gateway WebSocket with tokio::select! heartbeat)
- **BroadcastLayer** tracing layer streams real-time logs to WebSocket subscribers
- **Ed25519 challenge-response** auth for secure WebSocket connections
- **Unified heartbeat scheduler** — per-agent cron/interval with meso/macro evolution, `max_concurrent_runs` semaphore
- **CronScheduler** reads `cron_tasks.jsonl`, evaluates cron expressions, fires tasks on schedule
- **Three-layer evolution** with real Claude subprocess calls: Micro (post-conversation) → Meso (per-agent heartbeat) → Macro (daily)
- **Security layer**: SOUL.md drift detection (SHA-256), prompt injection scanner (6 rule categories), JSONL audit log, per-agent key isolation
- **Behavioral contracts** (`CONTRACT.toml`) with `must_not` / `must_always` boundaries + `duduclaw test` red-team CLI
- **Skill ecosystem**: OpenClaw-compatible skill parser (YAML frontmatter), local skill registry with weighted search, MCP `skill_search` / `skill_list` tools
- **API key encryption**: AES-256-GCM stored as base64 in config

## Design Context

### Users
DuDuClaw is a **Claude Code extension layer** for individual developers and power users, primarily in Taiwan (zh-TW). Users interact through a web dashboard to manage AI agents, monitor channels (LINE/Telegram/Discord), track API budgets, and observe agent self-evolution. They expect a tool that feels like a trusted companion — not a cold enterprise console.

### Brand Personality
**Professional · Efficient · Precise** — with a warm, approachable surface.

Like a skilled engineer who happens to be your close friend: reliable, sharp, but never cold. The paw print (🐾) icon reflects a pet-like companionship — the AI is loyal, attentive, and delightful to interact with.

### Aesthetic Direction
- **Primary references**: Claude.ai (warm sand/beige tones, generous whitespace, soft typography) + Raycast (macOS-native polish, frosted glass effects, refined dark theme)
- **Anti-references**: Grafana (too dense), Discord (too playful), enterprise dashboards (too cold)
- **Color palette**:
  - Primary: warm amber (`amber-500` / `#f59e0b`) — evokes warmth and trust
  - Accent: soft orange (`orange-400` / `#fb923c`) — for highlights and CTAs
  - Surface light: warm stone (`stone-50` / `#fafaf9`) with subtle warm undertones
  - Surface dark: deep stone (`stone-900` / `#1c1917`) — warm dark, not cold blue-black
  - Success: emerald, Warning: amber, Error: rose — standard semantic colors
- **Theme**: Follow system preference (auto dark/light), with manual toggle
- **Typography**: System font stack for performance; generous line-height; larger body text (16px base)
- **Border radius**: Rounded (0.75rem default) — soft, approachable
- **Spacing**: Generous padding — the interface should breathe
- **Motion**: Subtle fade-in/slide transitions (150-200ms); respect `prefers-reduced-motion`
- **Glass effects**: Subtle backdrop-blur on sidebars and overlays (Raycast influence)

### Design Principles
1. **Warmth over sterility** — Every surface should feel inviting. Prefer warm neutrals over cold grays. Use color strategically to create emotional connection.
2. **Clarity over density** — Show what matters, hide what doesn't. Progressive disclosure: summary first, details on demand. Never overwhelm.
3. **Real-time without anxiety** — Status indicators should inform, not alarm. Use gentle transitions for state changes. Green means "all is well" and should be the dominant state color.
4. **One binary, one experience** — The dashboard is embedded in the Rust binary. It should feel native and instant, like a local app, not a remote web service.
5. **Accessible by default** — WCAG 2.1 AA compliance. Semantic HTML. Keyboard navigation. Respect motion preferences. Sufficient color contrast in both themes.
