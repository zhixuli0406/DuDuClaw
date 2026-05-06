"""
[W22-Spike-P1] Managed Agents Files API × Wiki 知識注入 PoC
=============================================================

目標
----
驗證 Anthropic Files API 作為 Wiki L0/L1 頁面傳輸層的可行性。

傳統做法（system prompt 文字注入）：
  每次 API 呼叫 → 把 L0+L1 頁面的全文貼進 system prompt → 佔用 context window

Files API 做法：
  1. 一次性將 wiki 頁面上傳為 Files API 的 file（POST /v1/files）
  2. 後續呼叫改用 file_id 在 user message 的 document block 引用
  3. Anthropic 伺服器端快取檔案內容 → 後續呼叫的 cache_read_tokens 激增

Spike 觀察指標
--------------
- 上傳延遲 (ms)
- 首次 call: cache_creation_tokens vs. input_tokens
- 後續 call: cache_read_tokens 比例
- 對比：純文字注入的 token 消耗

使用方式
--------
  export ANTHROPIC_API_KEY=sk-ant-...
  python poc_files_api.py

  # 或指定 wiki 目錄
  python poc_files_api.py --wiki-dir ~/.duduclaw/shared/wiki
"""

from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

logging.basicConfig(level=logging.INFO, format="%(levelname)s %(message)s")
logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Constants
# ---------------------------------------------------------------------------

# Anthropic Files API beta header
FILES_API_BETA = "files-api-2025-04-14"

# Prompt caching beta header
PROMPT_CACHE_BETA = "prompt-caching-2024-07-31"

# Default model for PoC
DEFAULT_MODEL = "claude-haiku-4-5"

# Mock wiki pages representing L0/L1 layers (used when no real wiki dir given)
MOCK_WIKI_PAGES: dict[str, str] = {
    "identity.md": """---
title: Agent Identity
layer: identity
trust: 1.0
tags: [identity, mission]
---

# DuDuClaw Agent Identity

I am an AI agent in the DuDuClaw platform. My mission is to assist users
with multi-agent orchestration, session management, and knowledge management.

## Core Values
- Precision and reliability over speed
- Transparent reasoning
- Proactive knowledge sharing across agents
""",
    "core-environment.md": """---
title: Core Environment
layer: core
trust: 1.0
tags: [core, environment, tooling]
---

# Core Environment Facts

## Platform
- Runtime: DuDuClaw v1.11.0
- Primary AI: Claude Sonnet / Haiku (via OAuth rotation)
- Storage: SQLite (sessions, wiki, tasks, evolution)
- Channels: Telegram, LINE, Discord, Slack

## Active Projects (W22 Sprint)
- ADR-002: x-duduclaw HTTP capability headers (DONE)
- Files API × Wiki injection PoC (in progress)
- Signed AgentCard v1.2 (W23)

## Critical Rules
- Never expose API keys in logs
- Wiki L0+L1 auto-injected; L2/L3 requires explicit search
- Max delegation depth: 5 layers
""",
    "core-team.md": """---
title: Team Roster
layer: core
trust: 0.9
tags: [core, team, roster]
---

# DuDuClaw Team Roster

| Agent | Role | Specialty |
|-------|------|-----------|
| TL-DuDuClaw | Tech Lead | Architecture, ADRs |
| ENG-AGENT | Sr Engineer | Agent systems, Files API |
| QA-AGENT | QA Lead | Test coverage, review |
| PM-AGENT | Project Manager | Sprint planning, OKRs |
""",
}


# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------

@dataclass
class TokenStats:
    """Token usage from a single API call."""
    input_tokens: int = 0
    cache_creation_tokens: int = 0
    cache_read_tokens: int = 0
    output_tokens: int = 0

    @property
    def total_billable(self) -> int:
        """Rough billable token count (cache_read is ~10x cheaper, simplified here)."""
        return self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens

    @property
    def cache_hit_rate(self) -> float:
        denom = self.input_tokens + self.cache_creation_tokens + self.cache_read_tokens
        return self.cache_read_tokens / denom if denom > 0 else 0.0

    def __str__(self) -> str:
        return (
            f"input={self.input_tokens} "
            f"cache_write={self.cache_creation_tokens} "
            f"cache_read={self.cache_read_tokens} "
            f"output={self.output_tokens} "
            f"(cache_hit={self.cache_hit_rate:.1%})"
        )


@dataclass
class FileRecord:
    """Uploaded file metadata."""
    file_id: str
    filename: str
    size: int
    upload_ms: float


@dataclass
class PocResult:
    """PoC experiment result for one approach."""
    approach: str
    call_1_stats: TokenStats = field(default_factory=TokenStats)
    call_2_stats: TokenStats = field(default_factory=TokenStats)
    call_3_stats: TokenStats = field(default_factory=TokenStats)
    upload_records: list[FileRecord] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)


# ---------------------------------------------------------------------------
# Wiki helpers
# ---------------------------------------------------------------------------

def load_wiki_pages(wiki_dir: Optional[Path]) -> dict[str, str]:
    """Load L0/L1 wiki pages from a directory, or return mock pages."""
    if wiki_dir is None or not wiki_dir.exists():
        logger.info("Using mock wiki pages (no real wiki dir found)")
        return MOCK_WIKI_PAGES

    pages: dict[str, str] = {}
    for md_file in wiki_dir.glob("*.md"):
        content = md_file.read_text(encoding="utf-8")
        # Simple layer detection from frontmatter
        layer = _extract_frontmatter_field(content, "layer")
        if layer in ("identity", "core", "l0", "l1"):
            pages[md_file.name] = content
            logger.info("  Loaded L0/L1 page: %s (%d bytes)", md_file.name, len(content))

    if not pages:
        logger.warning("No L0/L1 pages found in %s, using mock pages", wiki_dir)
        return MOCK_WIKI_PAGES

    logger.info("Loaded %d wiki pages from %s", len(pages), wiki_dir)
    return pages


def _extract_frontmatter_field(content: str, field: str) -> str:
    """Extract a field value from YAML frontmatter (simplified parser)."""
    in_frontmatter = False
    for i, line in enumerate(content.splitlines()):
        if i == 0 and line.strip() == "---":
            in_frontmatter = True
            continue
        if in_frontmatter and line.strip() == "---":
            break
        if in_frontmatter and line.startswith(f"{field}:"):
            return line.split(":", 1)[1].strip().strip('"').strip("'")
    return ""


def pages_to_single_document(pages: dict[str, str]) -> str:
    """Concatenate all pages into a single wiki document for upload."""
    sections = []
    for filename, content in sorted(pages.items()):
        sections.append(f"<!-- FILE: {filename} -->\n{content}\n")
    return "\n---\n".join(sections)


# ---------------------------------------------------------------------------
# Files API operations
# ---------------------------------------------------------------------------

def upload_wiki_as_file(client, pages: dict[str, str]) -> FileRecord:
    """Upload all wiki L0/L1 pages as a single text file to Files API."""
    combined = pages_to_single_document(pages)
    content_bytes = combined.encode("utf-8")

    logger.info("Uploading wiki bundle (%d bytes) to Files API...", len(content_bytes))
    t0 = time.perf_counter()

    # The Files API expects a file-like object or tuple (filename, content, mime_type)
    response = client.beta.files.upload(
        file=("wiki-knowledge-bundle.md", content_bytes, "text/markdown"),
    )

    upload_ms = (time.perf_counter() - t0) * 1000
    record = FileRecord(
        file_id=response.id,
        filename="wiki-knowledge-bundle.md",
        size=len(content_bytes),
        upload_ms=upload_ms,
    )
    logger.info(
        "  Upload done: file_id=%s  size=%d bytes  latency=%.0fms",
        record.file_id, record.size, record.upload_ms,
    )
    return record


def delete_file(client, file_id: str) -> None:
    """Clean up an uploaded file after the PoC."""
    try:
        client.beta.files.delete(file_id)
        logger.info("Deleted file: %s", file_id)
    except Exception as e:
        logger.warning("Could not delete file %s: %s", file_id, e)


# ---------------------------------------------------------------------------
# API call helpers
# ---------------------------------------------------------------------------

TEST_QUESTIONS = [
    "What is the agent's primary mission and which platform version is running?",
    "List the active projects in the current sprint.",
    "Who is the Tech Lead in the team roster?",
]


def _extract_stats(usage) -> TokenStats:
    """Extract TokenStats from Anthropic SDK usage object.

    Note: SDK fields are Optional[int] — a present but None value would bypass
    the getattr default. Using `or 0` ensures we always get an int.
    """
    return TokenStats(
        input_tokens=getattr(usage, "input_tokens", 0) or 0,
        cache_creation_tokens=getattr(usage, "cache_creation_input_tokens", 0) or 0,
        cache_read_tokens=getattr(usage, "cache_read_input_tokens", 0) or 0,
        output_tokens=getattr(usage, "output_tokens", 0) or 0,
    )


def call_with_text_injection(
    client,
    model: str,
    wiki_pages: dict[str, str],
    question: str,
) -> TokenStats:
    """Traditional approach: inject wiki text directly into system prompt."""
    wiki_text = pages_to_single_document(wiki_pages)
    system_prompt = f"""You are a DuDuClaw agent with access to the following wiki knowledge base:

<wiki_knowledge>
{wiki_text}
</wiki_knowledge>

Use the wiki knowledge to answer user questions accurately."""

    response = client.messages.create(
        model=model,
        max_tokens=256,
        system=[
            {
                "type": "text",
                "text": system_prompt,
                "cache_control": {"type": "ephemeral"},
            }
        ],
        messages=[{"role": "user", "content": question}],
        extra_headers={"anthropic-beta": PROMPT_CACHE_BETA},
    )
    return _extract_stats(response.usage)


def call_with_files_api(
    client,
    model: str,
    file_id: str,
    question: str,
) -> TokenStats:
    """Files API approach: reference uploaded wiki file in user message."""
    # The wiki file is referenced as a document block in the user message.
    # This allows Anthropic to cache the file content server-side.
    response = client.beta.messages.create(
        model=model,
        max_tokens=256,
        system=(
            "You are a DuDuClaw agent. The wiki knowledge base is provided "
            "as a document in the user message. Use it to answer questions accurately."
        ),
        messages=[
            {
                "role": "user",
                "content": [
                    # Inject wiki knowledge via Files API reference
                    {
                        "type": "document",
                        "source": {
                            "type": "file",
                            "file_id": file_id,
                        },
                        "title": "DuDuClaw Wiki Knowledge Base (L0+L1)",
                        "cache_control": {"type": "ephemeral"},
                    },
                    # The actual question
                    {
                        "type": "text",
                        "text": question,
                    },
                ],
            }
        ],
        betas=[FILES_API_BETA, PROMPT_CACHE_BETA],
    )
    return _extract_stats(response.usage)


# ---------------------------------------------------------------------------
# PoC Experiment Runner
# ---------------------------------------------------------------------------

def run_text_injection_experiment(
    client,
    model: str,
    wiki_pages: dict[str, str],
) -> PocResult:
    """Run 3 consecutive calls using traditional text injection."""
    result = PocResult(approach="Text Injection (System Prompt)")
    stats_attrs = ["call_1_stats", "call_2_stats", "call_3_stats"]

    for i, (attr, question) in enumerate(zip(stats_attrs, TEST_QUESTIONS), 1):
        logger.info("[Text Injection] Call %d: %s", i, question[:50])
        try:
            stats = call_with_text_injection(client, model, wiki_pages, question)
            setattr(result, attr, stats)
            logger.info("  Stats: %s", stats)
        except Exception as e:
            result.errors.append(f"Call {i}: {e}")
            logger.error("  Error: %s", e)

    return result


def run_files_api_experiment(
    client,
    model: str,
    wiki_pages: dict[str, str],
) -> PocResult:
    """Upload wiki to Files API then run 3 consecutive calls."""
    result = PocResult(approach="Files API (Document Reference)")

    # Upload once
    try:
        record = upload_wiki_as_file(client, wiki_pages)
        result.upload_records.append(record)
    except Exception as e:
        result.errors.append(f"Upload failed: {e}")
        logger.error("Upload failed: %s", e)
        return result

    file_id = record.file_id
    stats_attrs = ["call_1_stats", "call_2_stats", "call_3_stats"]

    try:
        for i, (attr, question) in enumerate(zip(stats_attrs, TEST_QUESTIONS), 1):
            logger.info("[Files API] Call %d: %s", i, question[:50])
            try:
                stats = call_with_files_api(client, model, file_id, question)
                setattr(result, attr, stats)
                logger.info("  Stats: %s", stats)
            except Exception as e:
                result.errors.append(f"Call {i}: {e}")
                logger.error("  Error: %s", e)
    finally:
        # Always clean up the uploaded file
        delete_file(client, file_id)

    return result


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------

def print_comparison_report(text_result: PocResult, files_result: PocResult) -> None:
    """Print a side-by-side comparison report."""
    separator = "=" * 70

    print(f"\n{separator}")
    print("  Managed Agents Files API × Wiki 知識注入 PoC — 實驗報告")
    print(separator)

    def total_stats(r: PocResult) -> TokenStats:
        s = TokenStats()
        for attr in ["call_1_stats", "call_2_stats", "call_3_stats"]:
            call = r.__dict__[attr]
            s.input_tokens += call.input_tokens
            s.cache_creation_tokens += call.cache_creation_tokens
            s.cache_read_tokens += call.cache_read_tokens
            s.output_tokens += call.output_tokens
        return s

    print("\n【傳統做法】文字注入 System Prompt")
    t = text_result
    for i, attr in enumerate(["call_1_stats", "call_2_stats", "call_3_stats"], 1):
        stats: TokenStats = t.__dict__[attr]
        print(f"  Call {i}: {stats}")
    t_total = total_stats(t)
    print(f"  合計: {t_total}")
    if t.errors:
        print(f"  ⚠ 錯誤: {'; '.join(t.errors)}")

    print("\n【Files API 做法】Document block 引用")
    f = files_result
    if f.upload_records:
        r = f.upload_records[0]
        print(f"  上傳: {r.filename} ({r.size} bytes, {r.upload_ms:.0f}ms) → {r.file_id}")
    for i, attr in enumerate(["call_1_stats", "call_2_stats", "call_3_stats"], 1):
        stats: TokenStats = f.__dict__[attr]
        print(f"  Call {i}: {stats}")
    f_total = total_stats(f)
    print(f"  合計: {f_total}")
    if f.errors:
        print(f"  ⚠ 錯誤: {'; '.join(f.errors)}")

    print(f"\n{separator}")
    print("  比較摘要 (3 次呼叫合計)")
    print(separator)

    def pct(a, b):
        if b == 0:
            return "N/A"
        return f"{(a - b) / b * 100:+.1f}%"

    print(f"  {'指標':<30} {'文字注入':>15} {'Files API':>15} {'差異':>12}")
    print(f"  {'-'*30} {'-'*15} {'-'*15} {'-'*12}")

    metrics = [
        ("input_tokens", "Input tokens"),
        ("cache_creation_tokens", "Cache write tokens"),
        ("cache_read_tokens", "Cache read tokens"),
        ("output_tokens", "Output tokens"),
        ("total_billable", "Total billable tokens"),
    ]

    for attr, label in metrics:
        tv = getattr(t_total, attr)
        fv = getattr(f_total, attr)
        diff = pct(fv, tv)
        print(f"  {label:<30} {tv:>15,} {fv:>15,} {diff:>12}")

    print(f"\n  Cache hit rate:")
    print(f"    文字注入:  {t_total.cache_hit_rate:.1%}")
    print(f"    Files API: {f_total.cache_hit_rate:.1%}")

    print(f"\n{separator}")
    print("  結論 & 建議")
    print(separator)

    if not f.errors and not t.errors:
        if f_total.cache_read_tokens > t_total.cache_read_tokens:
            print("  ✅ Files API 的 cache_read_tokens 高於文字注入")
            print("     → 後續呼叫的 token 成本顯著降低")
        else:
            print("  ⚠ 本次 cache_read_tokens 未顯著提升")
            print("     → 可能需要更多呼叫才能看到明顯差異（快取需要時間暖機）")

        print("\n  建議整合方向:")
        print("  1. WikiFilesCache (Rust) — 維護 page_name → file_id 映射表")
        print("  2. 修改 direct_api.rs — 支援 document block 參考 file_id")
        print("  3. L0/L1 頁面變更時觸發重新上傳 (TTL: 24h 或 content hash 驅動)")
        print("  4. Beta 期間: 同時保留 system prompt fallback")
    else:
        print("  ⚠ 實驗有錯誤，結果不完整，請檢查 API key 與 beta 功能可用性")

    print(f"\n{separator}\n")


def save_results(
    text_result: PocResult,
    files_result: PocResult,
    output_path: Path,
) -> None:
    """Save results to JSON for further analysis."""
    def result_to_dict(r: PocResult) -> dict:
        return {
            "approach": r.approach,
            # Single dict keyed by call number, not a list of single-entry dicts
            "calls": {
                f"call_{i}": getattr(r, f"call_{i}_stats").__dict__
                for i in [1, 2, 3]
            },
            "upload_records": [rec.__dict__ for rec in r.upload_records],
            "errors": r.errors,
        }

    data = {
        "text_injection": result_to_dict(text_result),
        "files_api": result_to_dict(files_result),
    }
    output_path.write_text(json.dumps(data, indent=2), encoding="utf-8")
    logger.info("Results saved to %s", output_path)


# ---------------------------------------------------------------------------
# Mock mode — simulates realistic API responses for CI/local verification
# ---------------------------------------------------------------------------

def _build_mock_client() -> object:
    """Build a mock Anthropic client that returns realistic token stats.

    Token values are based on DuDuClaw wiki bundle (~800 tokens) + Haiku pricing.
    Call 1: cold cache write (cache_creation > 0)
    Call 2/3: cache hit (cache_read > 0, input drops to near zero)
    """
    from unittest.mock import MagicMock

    client = MagicMock()

    # --- Text injection mock responses ---
    # System prompt contains ~800 wiki tokens + 50 system preamble + ~30 user q
    def _text_inject_call_stats(call_num: int) -> MagicMock:
        usage = MagicMock()
        if call_num == 1:
            # First call: cache miss → cache_creation_input_tokens filled
            usage.input_tokens = 60
            usage.cache_creation_input_tokens = 850
            usage.cache_read_input_tokens = 0
            usage.output_tokens = 42
        else:
            # Subsequent calls: cache hit → cache_read populated
            usage.input_tokens = 35
            usage.cache_creation_input_tokens = 0
            usage.cache_read_input_tokens = 850
            usage.output_tokens = 38
        return usage

    text_responses = [MagicMock() for _ in range(3)]
    for i, resp in enumerate(text_responses, 1):
        resp.usage = _text_inject_call_stats(i)

    client.messages.create.side_effect = text_responses

    # --- Files API mock responses ---
    # File upload
    upload_resp = MagicMock()
    upload_resp.id = "file_mock_a1b2c3d4e5f6"
    client.beta.files.upload.return_value = upload_resp
    client.beta.files.delete.return_value = None

    def _files_api_call_stats(call_num: int) -> MagicMock:
        usage = MagicMock()
        if call_num == 1:
            # First call with file_id: Anthropic caches file content server-side
            # input_tokens is LOWER (no wiki text in message text)
            usage.input_tokens = 35
            usage.cache_creation_input_tokens = 820
            usage.cache_read_input_tokens = 0
            usage.output_tokens = 41
        else:
            # Cache hit on subsequent calls
            usage.input_tokens = 35
            usage.cache_creation_input_tokens = 0
            usage.cache_read_input_tokens = 820
            usage.output_tokens = 37
        return usage

    files_responses = [MagicMock() for _ in range(3)]
    for i, resp in enumerate(files_responses, 1):
        resp.usage = _files_api_call_stats(i)

    client.beta.messages.create.side_effect = files_responses

    return client


def run_mock_mode(
    model: str,
    wiki_pages: dict[str, str],
    skip_text_injection: bool,
    skip_files_api: bool,
    output: str,
) -> int:
    """Run the full PoC experiment against a mock client (no API key needed).

    Validates:
    - Experiment runner orchestration logic
    - Report generation correctness
    - JSON output format
    Simulates realistic token distributions from Haiku model observations.
    """
    logger.info("=== [MOCK MODE] Managed Agents Files API × Wiki 知識注入 PoC ===")
    logger.info("Model: %s (simulated)", model)
    logger.info("Wiki pages: %d", len(wiki_pages))
    logger.info("Total wiki size: %d bytes", sum(len(v) for v in wiki_pages.values()))
    logger.info("")
    logger.info("⚠ Using mock API responses — token numbers are realistic simulations,")
    logger.info("  not live Anthropic API data. Set ANTHROPIC_API_KEY to run live.")
    logger.info("")

    client = _build_mock_client()

    text_result = PocResult(approach="Text Injection (System Prompt) [MOCK]")
    files_result = PocResult(approach="Files API (Document Reference) [MOCK]")

    if not skip_text_injection:
        logger.info("--- Experiment 1: Text Injection (mock) ---")
        text_result = run_text_injection_experiment(client, model, wiki_pages)
        logger.info("")

    if not skip_files_api:
        logger.info("--- Experiment 2: Files API (mock) ---")
        files_result = run_files_api_experiment(client, model, wiki_pages)
        logger.info("")

    print_comparison_report(text_result, files_result)
    output_path = Path(output)
    save_results(text_result, files_result, output_path)
    return 0


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------

def main() -> int:
    parser = argparse.ArgumentParser(
        description="Files API × Wiki 知識注入 PoC"
    )
    parser.add_argument(
        "--wiki-dir",
        help="Path to wiki directory (L0/L1 pages). Uses mock pages if not set.",
    )
    parser.add_argument(
        "--model",
        default=DEFAULT_MODEL,
        help=f"Anthropic model to use (default: {DEFAULT_MODEL})",
    )
    parser.add_argument(
        "--skip-text-injection",
        action="store_true",
        help="Skip the text injection experiment (only run Files API)",
    )
    parser.add_argument(
        "--skip-files-api",
        action="store_true",
        help="Skip the Files API experiment (only run text injection)",
    )
    parser.add_argument(
        "--output",
        default="poc_results.json",
        help="Output JSON file for results (default: poc_results.json)",
    )
    parser.add_argument(
        "--mock",
        action="store_true",
        help=(
            "Run with simulated API responses (no ANTHROPIC_API_KEY needed). "
            "Validates experiment orchestration and report generation logic."
        ),
    )
    args = parser.parse_args()

    # Load wiki pages (same for both real and mock mode)
    wiki_dir = Path(args.wiki_dir) if args.wiki_dir else None
    wiki_pages = load_wiki_pages(wiki_dir)

    # --- Mock mode: skip API key check ---
    if args.mock:
        return run_mock_mode(
            model=args.model,
            wiki_pages=wiki_pages,
            skip_text_injection=args.skip_text_injection,
            skip_files_api=args.skip_files_api,
            output=args.output,
        )

    # --- Real mode: require API key ---
    api_key = os.environ.get("ANTHROPIC_API_KEY", "").strip()
    if not api_key:
        print(
            "ERROR: ANTHROPIC_API_KEY not set.\n"
            "       Set it to run against live Anthropic API, or use --mock for simulation.",
            file=sys.stderr,
        )
        return 1

    # Import Anthropic SDK
    try:
        import anthropic
    except ImportError:
        print("ERROR: anthropic package not installed. Run: pip install anthropic", file=sys.stderr)
        return 1

    client = anthropic.Anthropic(api_key=api_key)

    logger.info("=== Managed Agents Files API × Wiki 知識注入 PoC ===")
    logger.info("Model: %s", args.model)
    logger.info("Wiki pages: %d", len(wiki_pages))
    logger.info("Total wiki size: %d bytes", sum(len(v) for v in wiki_pages.values()))
    logger.info("")

    text_result = PocResult(approach="Text Injection (System Prompt)")
    files_result = PocResult(approach="Files API (Document Reference)")

    # Run experiments
    if not args.skip_text_injection:
        logger.info("--- Experiment 1: Text Injection ---")
        text_result = run_text_injection_experiment(client, args.model, wiki_pages)
        logger.info("")

    if not args.skip_files_api:
        logger.info("--- Experiment 2: Files API ---")
        files_result = run_files_api_experiment(client, args.model, wiki_pages)
        logger.info("")

    # Print comparison report
    print_comparison_report(text_result, files_result)

    # Save results
    output_path = Path(args.output)
    save_results(text_result, files_result, output_path)

    return 0


if __name__ == "__main__":
    sys.exit(main())
