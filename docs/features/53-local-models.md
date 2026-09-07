# Local models on the device

> Download one model, switch it on, and the AI keeps answering with the network unplugged.

---

## What this covers

Feature [#45](45-local-model-marketplace.md) is the open-ended Hugging Face
marketplace: search any repo, any quantization, install what you like. This
page covers the narrower thing a DuDuClaw OS device does out of the box —
a short verified list, one click to download, one click to make a
downloaded model the live one.

The device image ships the inference engine but no weights. Weights are
gigabytes and they go stale, so they are fetched on demand instead of
being frozen into the image.

---

## Six models, checked by hand

Every row in the built-in list had its repository id, file name, byte size
and anonymous downloadability checked against the live Hugging Face API on
2026-09-05. Nothing here is a guess, and two of the checks changed the
list:

- Qwen's own `Qwen3-1.7B-GGUF` repository ships no Q4_K_M at all, only a
  Q8_0. The 1.7B row therefore comes from `unsloth`.
- Google's Gemma 3 GGUF repository requires manual approval before it will
  serve a file, which makes one-click download impossible. The ungated
  `ggml-org` build is used instead.

| Model | Size | Memory | Good for |
|-------|------|--------|----------|
| Qwen3 1.7B | 1.0 GB | ~2.5 GB | The smallest thing that still holds a conversation; strong Chinese |
| Llama 3.2 3B | 1.9 GB | ~3.4 GB | English chat on a small machine |
| Qwen3 4B | 2.3 GB | ~3.9 GB | The balanced default |
| Gemma 3 4B | 2.3 GB | ~3.9 GB | Multilingual chat |
| Qwen2.5-Coder 7B | 4.4 GB | ~5.9 GB | Writing and reading code |
| Qwen3 8B | 4.7 GB | ~6.2 GB | The most capable of the six |

All six are Q4_K_M. The memory column is the file size plus room for the
context window and the engine's working buffers, so it is what the machine
actually needs while answering, not what the download weighs.

A local model also has a much smaller context window than a cloud model — the appliance serves 8192 tokens by default. When the agent's tool loop runs against the local engine, the gateway asks llama.cpp for that window (`/props`) and fits the request into it: tools are kept in registry order until 45% of the budget is used (the `tasks_*` tools the goal loop relies on are always kept), the system prompt is cut from the tail with a visible marker if it still does not fit, and 1024 tokens stay free for the answer. Without this a full agent prompt plus every MCP tool (~33 k tokens) was rejected by the server on every round.

---

## Three steps

Open **Manage → Local models**. The built-in panel sits above the
marketplace.

```
   +---------------------------+
   |  Pick a model             |   green / amber / red fit light,
   |                           |   computed for THIS machine now
   +------------+--------------+
                |
                v
   +---------------------------+
   |  Download                 |   runs in the background, resumable,
   |                           |   progress on the card
   +------------+--------------+
                |
                v
   +---------------------------+
   |  Use this model           |   saves the setting, starts the engine,
   |                           |   banner turns green when it answers
   +---------------------------+
```

The fit light compares the model's memory requirement against what the
machine has free right now. Green means comfortable, amber means it will
fit with little room to spare, red means it will not fit.

---

## What the banner tells you

The status line is derived from a live probe, never from what is written in
the settings. "Local model is answering" means the engine responded to a
request a moment ago and named the weights it has loaded. If it went down,
the banner says so on the next poll.

Switching models is the same two clicks: download the other one, press
**Use this model**, and the engine restarts against the new file. The
previous file stays on disk until you delete it from the installed list.

---

## Honest limits

**Speed.** Answers are computed by the device's own processor and
integrated graphics. A small local model feels noticeably slower than a
cloud model, and how much slower depends entirely on your hardware. The
dashboard deliberately shows no tokens-per-second figure, because nobody
has measured one on your machine.

**Capability.** A 4B model is not a substitute for a frontier cloud model
on hard reasoning, long multi-step work, or code review. It is a good fit
for the short, repetitive turns that make up most of a day: classify this,
summarize that, translate this line, answer a routine question.

**Nothing is fabricated.** A device with no weights downloaded reports
exactly that. Local inference refuses up front with "no local model
installed" rather than issuing a request that is guaranteed to fail and
surfacing a connection error nobody can act on.

---

## How the cloud and the local model share the work

The device keeps `inference_mode = "hybrid"` by default. A configured cloud
runtime still handles the work it is better at; the local model is
available underneath it, not in front of it. Two ways to lean on the local
model more:

- Per agent, set `prefer_local = true` under `[model.local]` in the agent's
  `agent.toml`. Simple turns go local first and fall back to the cloud when
  the local path fails.
- Globally, set `inference_mode = "local"` in `config.toml`. Every request
  goes to the local model, and the cloud is only reached if the confidence
  router escalates.

With no cloud account configured at all, the local model becomes the only
path — which is what makes an unplugged device still useful.

For the confidence router that decides *which* requests are worth
escalating, see [#03](03-confidence-router.md). For searching beyond the
six built-in models, see [#45](45-local-model-marketplace.md).

---

## Where things live

- Downloaded weights: the device's own model directory, alongside anything
  installed from the marketplace.
- The active selection: written whenever you press **Use this model**, and
  read back by the status panel.

Both are inside the device's data area, so they survive a system update.
