# Twitter/X Thread Templates for DuDuClaw

## Template 1: Architecture Deep Dive

**Tweet 1 (Hook):**
I built a 12-crate Rust system that lets AI agents evolve themselves — 90% of conversations cost $0.

Here's how it works 🧵

**Tweet 2:**
The core insight: most customer questions are predictable.

DuDuClaw uses Prediction-Driven Evolution (Active Inference + Dual Process Theory) to handle ~90% of conversations with zero LLM calls.

Only novel/complex queries hit the API.

**Tweet 3:**
The architecture: 12 Rust crates covering:
- 3-channel routing (LINE/Telegram/Discord)
- Multi-backend local inference (llama.cpp/mistral.rs/Exo P2P)
- GVU self-play evolution loop
- Ed25519 security + prompt injection scanning

**Tweet 4:**
Local inference stack:
- llama.cpp (Metal/CUDA/Vulkan)
- mistral.rs (PagedAttention + Speculative Decoding)
- Exo P2P cluster (235B+ models across machines)
- Confidence Router: auto-routes by query complexity

**Tweet 5 (CTA):**
Open source under Elastic License 2.0.
⭐ GitHub: [link]
💬 Discord: [link]

#BuildInPublic #RustLang #AIAgents #ClaudeCode

---

## Template 2: Zero-Cost Challenge

**Tweet 1:**
Day {N} of the Zero-Cost AI Challenge:
- Conversations today: {X}
- API cost: $0.00
- Auto-reply rate: {Y}%
- Local inference: llama.cpp on Mac Mini M2

The Evolution Engine keeps learning. 📈

---

## Template 3: Customer Story

**Tweet 1:**
A restaurant owner asked: "Can AI handle my LINE customer service?"

3 hours later:
✅ Menu FAQ loaded
✅ Reservation handling
✅ 24/7 auto-reply
✅ Running on a Mac Mini under the counter

Total monthly cost: $0.

Thread on how we set it up 👇

---

## Hashtags Pool

Core: #BuildInPublic #DuDuClaw #AIAgents
Tech: #RustLang #ClaudeCode #MCP #LocalLLM
Market: #AItools #SelfHosted #OpenSource
Taiwan: #TaiwanTech #台灣AI #數位轉型
