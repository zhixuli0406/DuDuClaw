# Local model marketplace

> Pick a model by purpose, see at a glance whether it fits this machine, and install it in one click.

---

## The barrier is terminology, not hardware

The hard part of running a local model was never the hardware. It's the vocabulary: GGUF, Q4_K_M, imatrix, context window, MoE. These words stand between "I want an AI that works offline" and actually getting one installed. The local model marketplace collapses that path into three steps anyone can read.

---

## How it works

Open **Manage → Local models** and choose what you want the model for. The marketplace scans Hugging Face across five quality-vetted publishers, and every recommendation carries a fit light (green / yellow / red) computed for **this machine**. Press install and the system picks the quantization for you, resumes interrupted downloads, and has the model ready to use the moment it lands.

```
Pick a purpose
      |
      v
+----------------------+
| Scan Hugging Face    |  <-- five vetted publishers,
|                      |      results cached 24h
+----------+-----------+
           |
           v
+----------------------+
| Compute the fit      |  <-- against this machine's
| light (🟢/🟡/🔴)      |      memory, right now
+----------+-----------+
           |
           v
+----------------------+
| One-click install    |  <-- best quant that fits,
|                      |      resumable download
+----------------------+
```

### Step 1: pick a purpose

Chat assistant, coding, long documents, or Chinese-first. No model names required.

### Step 2: read the fit light

| Light | What it means |
|-------|---------------|
| 🟢 Runs comfortably | Memory footprint stays under 60%; everyday use won't disturb other work |
| 🟡 Barely fits | It loads, but it's tight — close other large applications first |
| 🔴 Won't fit | This variant exceeds what this machine can handle |

The verdict comes from the actual file size, plus the cache reservation needed at inference time, plus runtime overhead, compared against 90% of the memory available right now. Not a theoretical figure — the state of this machine, today.

### Step 3: install with one click

The system picks the highest-quality quantization that fits (imatrix-calibrated builds preferred), resumes the download if the connection drops, and the model appears in the installed list when done.

---

## Dual-track fit for MoE models: running 30B on a 16GB machine

MoE (Mixture-of-Experts) models like 30B-A3B carry 30B total parameters but activate only 3B per token. Their expert weights do **not** need to sit in fast memory all at once — the turbo-fieldfare project demonstrated a 26B model running with just 2GB resident. The marketplace therefore shows two lights for MoE models:

| Track | What it judges |
|-------|----------------|
| **Full load** | The traditional verdict: the whole package loaded into memory |
| **VRAM-saving mode** | Only shared layers go to the GPU; experts stay in system memory |

A model that is red on full load but green in VRAM-saving mode gets a "VRAM-saving mode available" label. The execution layer can enable it today: `[llamafile] extra_args = ["--cpu-moe"]` in `inference.toml` (llamafile is llama.cpp underneath). Once llama.cpp's native expert SSD streaming (upstream PR #25294) merges, it will follow as a single toggle.

---

## For power users

- **Advanced drawer**: hand-pick from every quantization variant of a model, with imatrix labels and a per-file fit light plus VRAM-saving verdict.
- **Manual install**: type any Hugging Face repo (`org/Model-GGUF`) and get the same quant enumeration and fit computation — the escape hatch that replaces the old list mechanism.
- **LoRA and other tweaks**: passed through `[llamafile] extra_args` in `inference.toml` (e.g. `["--lora", "/path/adapter.gguf"]`), editable on the inference settings page.
- **HF token**: set the `HF_TOKEN` environment variable to double the rate limit and reach gated models.

---

## Data sources and quality

Five publishers make the whitelist: unsloth (strong on MoE), bartowski, mradermacher, lmstudio-community, and ggml-org — selected on community quantization-quality benchmarks (KL divergence against the original weights). When several publishers ship the same model, duplicates collapse to the best one. Search results are cached for 24 hours; if Hugging Face is unreachable, the marketplace falls back to the cache and never blocks the page.

---

## Boundaries

- **The model files are the registry.** Install means a file lands in `~/.duduclaw/models/`; delete means the file is removed. The agent-facing `model_list` / `model_load` MCP tools read the same directory — their behavior is unchanged.
- **The inference settings page stays put.** Backend selection, routing, and llamafile arguments remain where they were. Only the "find a model, install a model" part moved here.

---

## The takeaway

The marketplace answers three questions in plain words: what is this model for, will it run on this machine, and how do I get it — a purpose picker, a fit light computed from real memory numbers, and a one-click resumable install.
