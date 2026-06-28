# Product Hunt launch — DuDuClaw

> **Launch window**: Tuesday 00:01 PT (= Taipei 16:01 Tuesday afternoon).
> Submit ~2 weeks ahead via PH's "Schedule a launch" flow.
> Sync to HN Show HN on the same day for compound first-day traffic.

---

## Hunter

Solo founders self-hunt to keep credit:

- **Maker**: zhixuli (Taiwan)
- **Hunter**: same person (self-hunt) — gives the post a credibility floor
  alternative: ask one of these established hunters if relationship permits
  - Chris Messina — old guard, posts at any reasonable hour
  - Kevin William David — high-volume professional hunter, takes 5–10% of his slot for unknown solo founders

---

## Name

```
DuDuClaw 🐾
```

(PH does support the paw emoji here, unlike HN.)

## Tagline (max 60 chars)

```
Self-hosted AI agents, not bound to any LLM provider.
```

Alternates:

```
Self-evolving AI agents in Rust. Apache 2.0 + commercial layer.
```

```
The plumbing your AI agent stack should already have.
```

## Topics (max 3)

1. Developer Tools
2. SaaS
3. Open Source

## Logo (240×240)

- Existing repo asset: `marketing/press-kit/logo-512.png` (TODO: extract from `web/public/favicon`)
- Background: soft amber (`#fef3c7`), centered paw print
- Avoid the gradient — PH compresses gradients into mud

## Gallery

> Up-vote-friendly order: GIFs first (autoplay catches eyes), screenshots
> after, social proof / architecture diagram last.

1. **30-second GIF — LINE → AI loop**
   Filename: `01-line-loop.gif`
   Caption: "Customer asks in LINE. DuDuClaw replies in 800ms. No third-party SaaS in the path."

2. **30-second GIF — Multi-runtime swap**
   Filename: `02-runtime-swap.gif`
   Caption: "Same agent, three backends: Claude → Codex → Gemini. One toml flag."

3. **Screenshot — Dashboard tier card**
   Filename: `03-license-page.png`
   Caption: "License page shows tier, unlocked modules, phone-home freshness."

4. **GIF — Evolution rollback in action**
   Filename: `04-evolution-rollback.gif`
   Caption: "SOUL.md proposes change → 4-layer verify → 24h observation → auto-rollback if metrics regress."

5. **Screenshot — Cost telemetry**
   Filename: `05-cost-telemetry.png`
   Caption: "Prediction engine kills ~90% of LLM calls before they cost anything."

6. **Diagram — Architecture overview**
   Filename: `06-architecture.png`
   Caption: "12 Rust crates + Python bridge. Each crate is independently testable."

## Description (max 260 chars on PH card; full description is longer)

### Card description

```
DuDuClaw is a self-hosted AI agent platform built in Rust. Unify Claude/Codex/Gemini behind one runtime, route LINE/Telegram/Discord messages, auto-evolve agent prompts, and burn 90% fewer LLM calls. Apache 2.0 core, commercial subscription for extras.
```

### Full description (about your product)

```
I've been building DuDuClaw for a year as a solo developer in Taiwan. Three things make it different from LangChain/Dify/n8n:

🎯 Not bound to one LLM provider
AgentRuntime trait abstracts Claude Code, Codex, Gemini, and OpenAI-compatible endpoints. When Anthropic broke OAuth subscriptions this year, I shipped a cross-platform PTY pool (ConPTY/openpty via portable-pty). Migration was per-agent: one line of toml.

🧠 Self-evolves through prediction error
Dual-process router skips the LLM call entirely when input resembles past dialogues — ~90% zero-cost in practice. Significant prediction errors trigger a GVU loop (Generator-Verifier-Updater) that proposes SOUL.md updates, runs them through 4-layer verification, and auto-rolls back if metrics regress after 24h.

📱 Multi-channel native, not bolted-on
LINE / Telegram / Discord built into the core. Per-channel agent routing. Discord uses real op-6 RESUME (not fresh IDENTIFY on every reconnect). Token routing cascades through the reports_to hierarchy so sub-agents can use parent's bot tokens.

The Apache 2.0 core gets you everything above. The NT$1,490/month commercial subscription pays for industry-tuned SOUL.md templates, Evolution best parameters, Dashboard Enterprise (audit + ROI exports), and a private Discord with the founder.

Built on:
• Rust (12 workspace crates) + Python bridge via PyO3
• SQLite per-agent (no cross-tenant query path = no cross-tenant leak)
• Ed25519-signed licenses with grace-period state machine
• AES-256-GCM RFC-23 redaction pipeline
• Docker / Apple Container agent sandboxing
• llama.cpp / mistral.rs / Exo P2P for local inference

Why I'd love your feedback:
1. Solo-founder economics — does the Apache 2.0 + commercial split read as honest, or as having-it-both-ways?
2. The multi-runtime trait — is it worth the abstraction cost vs locking to Claude?
3. SOUL.md auto-evolution — does the 24h observation + rollback story land?

Roast the design. I'd rather hear it now than after the next 50 paying customers.
```

## First-day social plan

| Hour (PT / Taipei) | Channel | Action |
|---|---|---|
| 00:01 PT / 16:01 TPE | PH | Launch goes live. Post-link tweet immediately. |
| 00:30 PT / 16:30 TPE | LINE OA | Newsletter to all subscribers: "今天上 Product Hunt 了，幫我衝一波" |
| 01:00 PT / 17:00 TPE | X / Twitter | Thread (10 tweets) telling the build story |
| 03:00 PT / 19:00 TPE | Discord | Post in DuDuClaw server + related Rust/AI communities |
| 06:30 PT / 22:30 TPE | HN | Submit Show HN (separate post — don't link PH) |
| 12:00 PT / 04:00 TPE | Reddit | r/rust + r/selfhosted submissions, customized angle each |
| Throughout | PH comments | Reply to every comment within 30 minutes for first 12 hours |

## Reply playbook for predictable comments

> Pre-write replies so you can ship them in <60 seconds.

### "How is this different from LangChain?"

> LangChain is a framework — you write Python that imports it, then deploy
> however you like. DuDuClaw is a platform — clone, configure, run. The
> overlap is small: I depend on a single Rust binary doing channel routing,
> agent lifecycle, and OAuth pool management. LangChain doesn't do any of
> those (you'd build them yourself or buy LangSmith).

### "How is this different from Dify / FastGPT?"

> Dify/FastGPT are SaaS-first products with self-host bolted on. DuDuClaw
> is self-host-first with a Cloud option. Concretely: no Postgres
> requirement, no Kubernetes assumption, one binary on a Mac Mini handles
> 50 simultaneous agents.

### "Why Rust?"

> Single-binary deployment, no GC pauses on hot paths (Discord gateway
> heartbeat is unforgiving), and tokio's async story is the best in any
> language right now for this kind of workload. The Python bridge handles
> ML-adjacent libraries (Whisper, MLX) so Rust isn't carrying that cost.

### "Why not 100% open source?"

> The core IS Apache 2.0 — every line that runs on a customer's machine,
> verifies a license, parses memory, drives an agent. The closed-source
> layer is the issuer side (signing key, control-plane, premium SOUL.md
> templates I built from 100+ real conversations). I wrote up the trade-off
> here: https://duduclaw.dudustudio.monster/blog/why-pay-for-apache-2

### "When are you raising?"

> Not raising. Solo, profitable from day-30 plan. The Apache 2.0 license
> is partly my insurance: if I disappear tomorrow, paying customers can
> fork and keep going. That's only credible if there are no investors
> behind me with conflicting interests.

---

## Post-launch follow-up

- **Day 1 evening**: Write a "Day 1 of PH launch — here's what I learned" blog post / X thread
- **Day 3**: Email all top-100 upvoters with a thank-you + 30% off code (one-time, transparent)
- **Day 7**: Write "What week 1 of paid PH traffic actually looked like" — be honest about churn
- **Day 30**: Quarterly post comparing PH paid-trial cohort to LINE OA paid-trial cohort
