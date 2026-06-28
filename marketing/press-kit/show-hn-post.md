# Show HN draft — DuDuClaw v1.16

> **Submission window**: Tuesday or Wednesday 06:30 PT (= Taipei 21:30 / 22:30).
> Avoid Mondays (low engagement) and Friday afternoons (weekend slump).
> Have the post written 24h ahead, then submit live so the first hour is real-time.

---

## Title (max 80 chars)

```
Show HN: DuDuClaw – Self-hosted multi-runtime AI agent platform (Rust, Apache 2)
```

Alternates if HN dupes detection trips:

```
Show HN: DuDuClaw – Unify Claude/Codex/Gemini CLI behind one AI agent stack (Rust)
```

```
Show HN: Self-evolving AI agents in Rust with multi-runtime, LINE/TG/Discord bots
```

## Body

> HN posts > 1500 chars get downvoted as spam. Stay under 1200.
> No emojis (HN hates them). No bullet abuse. One link. Plain text.

```
Hi HN — I'm a solo developer in Taiwan. DuDuClaw is a Rust-based AI agent platform I've been building for a year. Two things make it different from the existing LangChain/Dify/n8n crowd:

1. It is NOT bound to a single LLM provider. There's a trait called AgentRuntime that abstracts Claude Code, Codex, Gemini, and any OpenAI-compatible HTTP endpoint. When Anthropic broke `claude -p` for OAuth-subscription accounts this year, I shipped a cross-platform PTY pool (ConPTY on Windows, openpty on Unix) that drives the real interactive REPL instead. Existing users got a transparent migration via per-agent `agent.toml [runtime] pty_pool_enabled = true`. The same trait layer also lets me ship a Claude → Codex fallback if Anthropic does something stupid again.

2. It self-evolves through prediction error. There's a dual-process router: when an incoming message resembles past dialogues (high prediction confidence), we skip the LLM call entirely. About 90% of conversations end up zero-cost in practice. Significant prediction errors trigger a GVU loop (Generator-Verifier-Updater) that proposes SOUL.md (system prompt) updates, runs them through 4-layer verification including an LLM judge, and either commits or rolls back. SOUL.md is version-controlled with 24h observation windows and automatic rollback if NPS metrics regress.

The plumbing around these two: SQLite memory engine with 3D-weighted retrieval (Generative Agents-style), Ed25519-signed license with grace-period state machine + signed CRL, AES-256-GCM redaction pipeline (RFC-23) so internal data gets <REDACT> tokens before leaving trusted boundaries, container sandbox for agent isolation, multi-OAuth account rotation with rate-limit/billing-aware cooldowns, Odoo ERP bridge with 15 MCP tools, and local inference via llama.cpp / mistral.rs / Exo P2P clusters.

Apache 2.0 core. Closed-source commercial layer ships separate (license, premium templates, dashboard enterprise, CRL signing infra). I wrote a separate post on why the bottom-of-stack stays open: https://duduclaw.dudustudio.monster/blog/why-pay-for-apache-2

GitHub: https://github.com/zhixuli0406/DuDuClaw

Genuinely curious what HN thinks about three specific design choices I had to make this year:

- Why I gave up on ELv2 and went back to Apache 2.0 (TL;DR: license-as-protection breaks the social contract that makes the open source ecosystem useful to me as a solo dev)

- Why I trust SQLite + per-agent DBs over PostgreSQL multi-tenant (TL;DR: cross-tenant query path doesn't exist if the rows live in different files)

- Why the PTY runtime defaults to OFF and is opt-in per agent (TL;DR: I'd rather break Anthropic OAuth users gradually than break API-key power-users immediately)

Roast away.
```

Word count: ~480 EN, well under the 1200 limit.

---

## First-hour comment seeds (Optional self-comments)

> HN ranks heavily on first-hour engagement. Don't astroturf, but DO prepare
> answers to predictable questions so you can reply instantly.

### Seed 1 (technical depth filter — invite the deeper crowd)

> Three implementation details that didn't fit the post:
>
> 1. AgentRuntime dispatch is compile-time monomorphized — no dyn fat pointer overhead. Adding Codex took ~600 LOC inside `crates/duduclaw-cli-runtime`.
> 2. The PTY pool uses sentinel-framed in-band response decoding instead of scrollback scraping. We synthesize an end-of-message sentinel, watch for it in the byte stream, and slice the response cleanly. portable-pty handles the OS abstraction; we own everything above.
> 3. The Evolution loop's L1-L2-L4 verification is pure-Rust deterministic logic (token overlap, format match, contract compliance). Only L3 — the LLM judge — costs anything. ~95% of GVU rounds reject before reaching L3.

### Seed 2 (one-person company angle — invite the indie hacker crowd)

> I'm running this as a solo founder, no funding, no investors. The Apache 2.0
> core is my honest commitment to customers: if I disappear tomorrow, the
> NT$1,490/month they're paying me today doesn't trap them in a dead product.
> I wrote up the business reasoning here: https://duduclaw.dudustudio.monster/blog/why-pay-for-apache-2

---

## Hard Don'ts

- No bold/italic — HN strips them and they look broken
- No tables — HN doesn't render them
- No "we / our team" language — single founder, own it
- No claim "the only / the first" — HN will fact-check you to death
- No emoji 🐾 (we live with the brand on the site, but not in the post)
- No screenshots in the post (only allowed: one URL)

---

## Pre-submission checklist

- [ ] GitHub README hero GIF is < 5 MB and shows the LINE chat → AI reply loop in 30 seconds or less
- [ ] Latest commit on `main` is green CI
- [ ] `cargo build --release` from a fresh clone completes in < 5 min
- [ ] Issues tab has no embarrassing TODOs as Title 1
- [ ] Two real users have starred (your friends count — but don't ask them to comment in the first 30 min)
- [ ] LINE OA is live; you can reply to any "how do I install" follow-up within 60 sec
- [ ] You can stay on HN for the first 6 hours without distraction (no meetings, no flight)

## Time-zone math

| HN Time | Taipei | Note |
|---|---|---|
| 06:30 PT Tue | 21:30 Tue | Best slot — US morning + EU evening |
| 09:00 PT Tue | 00:00 Wed | Good — US peak engagement |
| 12:00 PT Tue | 03:00 Wed | Skip — you'll be asleep, comments unanswered = death |
| 06:30 PT Wed | 21:30 Wed | Backup |
