# DuDuClaw Python SDK

Python companion package for [DuDuClaw](https://github.com/zhixuli0406/DuDuClaw) — the Multi-Agent AI Assistant Platform.

This package carries the Python-side helpers that sit next to the DuDuClaw Rust binary:

- **`duduclaw.mcp`** — MCP server helpers: API-key auth with scope enforcement, memory tools (store / read / search / namespace / quota)
- **`duduclaw.agents`** — capability-based agent routing (manifest loader, matcher, router, memory resolver)
- **`duduclaw.evolution`** — evolution vetter (GVU self-play verification)
- **`duduclaw.tools`** — agent tool definitions

## Installation

```bash
pip install duduclaw
```

> **Note:** This package is a companion to the main DuDuClaw binary.
> Install the binary via npm (`npx duduclaw`) or the install script — see the repository README.

## Requirements

- Python 3.10+
- The `anthropic` and `httpx` packages (installed automatically)

## Memory evaluation harness (repository only)

`python/duduclaw/memory_eval/` is a benchmarking harness for memory retrieval quality. It is **not part of the published wheel** — no runtime path calls it, and shipping 7k lines of benchmark code to every `pip install` bought nobody anything. Run it from a checkout instead:

```bash
git clone https://github.com/zhixuli0406/DuDuClaw && cd DuDuClaw
# The harness needs four packages the published wheel does not depend on:
pip install aiohttp asyncpg datasets pytest pytest-asyncio aioresponses
PYTHONPATH=python python -m duduclaw.memory_eval.smoke_test         # ~5 min P0 smoke run
PYTHONPATH=python pytest python/duduclaw/memory_eval/tests -v       # the harness's own tests
```

Without `asyncpg`, `aiohttp`, and `datasets` the modules will not even import — one more reason they have no business in a wheel whose declared dependencies are `anthropic`, `httpx`, and `pyyaml`.

About its data: `data/golden_qa_set.jsonl` holds **200 hand-written question/answer pairs** (every row is tagged `source: "manual"`). It is a DuDuClaw-authored zh-TW set, **not** LOCOMO and not derived from it, despite what some older internal notes said. `fetch_benchmarks.py` can pull LongMemEval and PersonaMem separately; those downloads are not bundled either.

## License

Apache-2.0
