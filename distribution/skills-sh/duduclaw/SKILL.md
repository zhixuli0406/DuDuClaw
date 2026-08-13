---
name: duduclaw
description: Use DuDuClaw — a self-hosted AI-employee platform — for cross-session memory, team-shared wiki knowledge, task boards, and messaging humans over LINE/Telegram/Discord/Slack. Applies when the user mentions DuDuClaw, asks their agent to remember things durably, or wants to reach people on messaging channels from an agent.
---

# DuDuClaw

DuDuClaw (https://github.com/zhixuli0406/DuDuClaw) runs the user's AI employees locally: channels, persistent memory, shared wiki, task board, HITL approvals.

## Connect (any MCP-capable agent)

Register the MCP server: command `npx`, args `["-y", "duduclaw", "mcp-server"]`. Claude Code users can instead add the plugin marketplace `zhixuli0406/duduclaw-claude-plugins`.

## Tool selection

- Durable personal memory → `memory_store` / `memory_search`
- Team-shared reference docs (SOPs, price lists) → `wiki_write` / `wiki_search`
- Work tracking → `tasks_create` / `tasks_list` / `tasks_complete`
- Message a human on their channels → `send_message`

## Rules

- Wiki for documents, memory for facts — never double-write.
- No secrets into memory/wiki.
- Tools absent ⇒ gateway not running: tell the user to start it (`duduclaw gateway`), don't silently skip.
