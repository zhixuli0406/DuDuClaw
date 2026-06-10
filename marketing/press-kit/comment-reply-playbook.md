# HN / PH Comment-reply playbook

> Pre-written responses to the predictable hostile or sceptical comments.
> Goal: reply in **<60 seconds** so the first hour stays warm.
> Tone: direct, technical, never defensive. Concede genuine criticism.

---

## Section 1 — Hostile / Bad-faith

### 1.1 "This is just another LangChain wrapper."

> It isn't. LangChain is an importable library. DuDuClaw is a deployable runtime. The overlap is one thing: both can talk to LLMs. Beyond that we're answering different questions — they ask "how do I chain calls in my Python code", we ask "how do I run an always-on customer-facing agent". Different problem, different stack, different deployment model.

### 1.2 "Why would I trust a solo developer with my production data?"

> You shouldn't, initially. That's why the core is Apache 2.0 — you can run the whole thing offline, in your VPC, on your laptop, without paying me a cent. Validate it on a non-critical channel for 30 days, then decide. The paid tier is for once you've already proven to yourself it works.

### 1.3 "Looks like vaporware. Where are the customers?"

> Two beta tenants, no paying customers publicly yet (this launch IS the public start). I'm leading with the technical depth on purpose — if you don't think the engineering holds up, the customer count was never going to convince you anyway. Look at the commits, look at the tests, decide.

### 1.4 "Rust is overkill for this."

> Three things needed Rust: (1) Discord Gateway heartbeat — GC pauses break that. (2) Multi-OAuth account rotation must be lock-free across concurrent agents. (3) PTY pool with sentinel-framed in-band decoding wants zero-copy parsing. I tried Go first (six months in). The async story for our shape of workload wasn't there. Rust + Tokio was.

### 1.5 "ELv2 is fine. Why did you go back to Apache 2.0?"

> ELv2 was a one-way ticket out of every Awesome list, every Reddit recommendation thread, every conservative IT department's procurement form. The "no hosted service" clause isn't enforceable against the actors I'm worried about (overseas reproduction) and IS enforceable against the actors who'd otherwise be friends (community deployers). Net negative.

### 1.6 "How is this not a hobby project?"

> The README has 1716 passing tests, twenty-something Rust crates, a workspace that builds in 90 seconds on a Mac Studio, and a year of commits at >2/day average. If "hobby project" means "someone takes it seriously", sure. If you mean "no commercial seriousness", I literally just shipped a working PayUni integration and an Ed25519-signed CRL infrastructure last week.

---

## Section 2 — Sceptical / Genuine Concerns

### 2.1 "Won't Anthropic break this again?"

> Almost certainly yes. They broke `claude -p` mid-2026 and I shipped a cross-platform PTY pool in response. That's the AgentRuntime trait's whole point: when one backend breaks, the other three still work. Customers using OAuth keep going via PTY; customers using API keys never noticed. The cost of betting wrong on this once would've sunk us; instead it became a feature.

### 2.2 "Local inference quality is terrible compared to Claude / GPT-4."

> For complex reasoning, yes. That's why local inference isn't the default — it's the Confidence Router's fallback when the prediction engine says "this is a known-class easy query". For "what time do you close" you don't need Claude. For "explain why this lease has an indemnification problem" you absolutely do. Confidence Router decides per query.

### 2.3 "What's stopping me from just patching out the license check?"

> Nothing. Apache 2.0 means you can. The Ed25519 check isn't trying to stop technical adversaries — it's the gating mechanism for the closed-source `commercial/` modules (premium templates, CRL infrastructure, audit log export). Patch out the check and you have the open-source build with empty hooks where the commercial modules would slot in. Which is fine — that's the customer who was never paying anyway.

### 2.4 "How do you handle data residency / GDPR?"

> Self-Host Pro: your data never leaves your machine. Period. Cloud: tenant containers live on a Mac Studio in Taipei + Hetzner VPS in Frankfurt + Singapore. EU customers get Frankfurt routing; rest of Asia gets Singapore. Backup goes to Hetzner Storage Box (Frankfurt, encrypted at rest). I'm not GDPR-compliant on paper — I'm honest about it on the privacy page. If you need an actual DPA, email me; I'll write you one specific to your data flow.

### 2.5 "Why no dependency on Postgres?"

> Two reasons. (1) Per-tenant SQLite makes cross-tenant query path impossible by construction — you can't write a buggy WHERE clause that leaks data across tenants if the tenants live in different files. (2) One-binary deployment story. Adding Postgres as a hard dep would kill the "brew install duduclaw, you're running" promise.

### 2.6 "Per-tenant SQLite doesn't scale."

> Correct, not infinitely. Empirically: 100 tenants on one Mac Studio M4 Max is comfortable. 500 is the redesign point. I'm not chasing 10,000 — that's a different product run by a different person at a different funding stage. Solo founder, 100 paying customers, NT$5 million annual revenue is the success scenario.

---

## Section 3 — Technical deep-dives (positive engagement)

### 3.1 "Tell me more about the prediction engine."

> Sure. The core idea is borrowed from Karl Friston's Active Inference / Bayesian brain framework, but I cut all the philosophy and kept the math. Every incoming message generates a feature vector (vocabulary novelty, sentiment delta, topic distance from agent's typical conversations). A simple linear predictor estimates "how surprising is this?" relative to the agent's historical conversation. Surprise above a calibrated threshold triggers an LLM call. Surprise below uses a template response. The threshold self-calibrates every 100 conversations against actual NPS feedback. In practice this kills ~90% of LLM calls because most customer questions are repeats.

### 3.2 "How does the GVU loop's L1-L2-L4 verification work?"

> Generator proposes a new SOUL.md diff. L1 checks token-level overlap with the previous version (rejects accidental complete rewrites). L2 checks format integrity (sections in right order, no broken templates). L4 evaluates against the CONTRACT.toml behavioural constraints — must-not / must-always rules that the agent SHALL NEVER violate. All three are deterministic Rust code, no LLM. If any reject, we never call L3 (the LLM judge). 95% of GVU proposals reject before L3, so the actual cost of running the evolution loop is essentially free.

### 3.3 "Show me your benchmarks."

> Skip the benchmark theatre. Here's what's verifiable:
>
> - `cargo test --workspace` runs 1716 tests in ~30s on Mac Studio
> - `cargo build --release` from cold cache is ~90s
> - Discord gateway latency to first heartbeat ack: typically <300ms
> - Per-message LLM cost reduction vs naive routing: 87% in our internal beta
>
> I'm not going to give you "DuDuClaw is N% faster than LangChain" because it's a category error — they don't do the same job.

### 3.4 "Why portable-pty for cross-platform PTY?"

> Existing options I evaluated:
> - tmux shim (maude) — Unix only, defeats the Windows goal
> - UDS PTY supervisor (torque) — Unix only, same problem
> - Direct conpty + openpty wrappers — would have had to write ConPTY bindings myself
>
> portable-pty's ConPTY support is the best Windows story without writing FFI. The cost is an extra dep, the benefit is one codebase shipping macOS/Linux/Windows.

---

## Section 4 — Pivot / Strategy questions

### 4.1 "Do you have a path to $100M ARR?"

> No. And I'd refuse one if it required raising. The path I have is NT$5-10 million annual revenue from 200-500 paying customers, fully owned by me, sustainable indefinitely. If your investing thesis requires $100M ARR, this isn't an investable company. That's deliberate.

### 4.2 "What happens if Anthropic acquires you?"

> They won't (they don't acquire one-person companies). And if they tried I'd say no — the whole point of multi-runtime is to NOT be at Anthropic's mercy. If they acquired me they'd own the moat they want broken.

### 4.3 "Is the Cloud business viable at this price?"

> At the gross level: yes. PayUni handles all the regulated stuff (cards, ATM, convenience store), takes 2.8% on domestic credit cards. NT$2,990/month Studio plan has ~NT$2,820 contribution margin. The constraint is not unit economics, it's customer acquisition — getting to 200 paying customers from a standing start is the actual challenge.

### 4.4 "You're underpricing yourself."

> Possibly. The pricing is based on "what would I pay if I were the customer". I'd pay NT$2,990 for Studio. I'd resent paying NT$8,000 for something I could replace by hiring an intern for two months. The price will go up when the moat is wider. Right now the moat is just my time, and my time is priced honestly.

---

## Section 5 — Rules of engagement (your own, not for posting)

1. **Reply to every comment in the first 6 hours.** Even "lol" gets a thoughtful response.
2. **Never reply to comments while angry.** Wait 5 minutes. Read it again. Then reply.
3. **Concede genuine criticism within 48 hours, publicly.** "You're right, I'll fix that" is the most powerful thing you can say on HN.
4. **Don't fight on PH.** Just reply technically and let the post fade — PH culture is much softer than HN.
5. **No DMs from the launch thread.** Anyone serious will email or LINE OA. DMs are a time-sink with no follow-through.
6. **Track every comment that turned into a customer.** If three Slack/HN comments → one paid trial, you've found a leading indicator.

---

## Section 6 — Comments that REQUIRE a response within 10 minutes

> If you miss any of these, the post is dead in the water.

- "The README link is broken" → fix and reply within minutes
- "I'm trying to install and X fails" → confirm OS + version, reproduce, reply
- "This is a fork of [other project]" → calmly link to your commit history
- "You're misrepresenting [tech detail]" → if true, correct + thank; if false, link to source
- "This violates [license / patent / law]" → respond once with facts, then disengage
