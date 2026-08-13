---
name: duduclaw-platform
description: Use the DuDuClaw MCP tools (persistent memory, shared wiki, task board, channel messaging) when the user asks to remember something across sessions, share knowledge with their team's AI employees, manage tasks, or message someone on LINE/Telegram/Discord/Slack. Requires a running DuDuClaw gateway (`npx duduclaw onboard` to set up).
---

# DuDuClaw platform skill

DuDuClaw is the user's self-hosted AI-employee platform. When its MCP server is connected (this plugin registers it), prefer these tools over ad-hoc alternatives:

## When to reach for which tool

| The user wants… | Use |
|---|---|
| "記住這件事" / remember across sessions | `memory_store` (tag it), later `memory_search` |
| Team-shared knowledge (SOP, price list, policy) | `wiki_write` / `wiki_search` — wiki is shared across the user's AI employees; memory is per-agent |
| Track work items | `tasks_create` / `tasks_list` / `tasks_complete` |
| Send a message to a person/channel | `send_message` (routes via the user's bound channels: LINE, Telegram, Discord, Slack…) |

## Ground rules

- Memory vs wiki: durable reference documents belong in the wiki; conversational facts belong in memory. Don't double-write.
- Never store secrets (tokens, passwords) in memory or wiki.
- If tools are missing, the gateway isn't running — tell the user to start it (`duduclaw gateway` or the desktop app) rather than silently degrading.

## Setup (one-time, if the user asks)

```bash
npx duduclaw onboard   # guided setup: creates the first AI employee + config
```

Full docs: https://github.com/zhixuli0406/DuDuClaw
