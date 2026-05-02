"""
memory_eval/build_golden_qa.py
Golden QA Set Seed Run 工具

功能：
  1. 對 golden_qa_set.jsonl 中 relevant_memory_ids 為空的查詢執行 seed run
  2. 以 memory_search 結果（similarity >= threshold）填入 relevant_memory_ids
  3. 輸出帶有填入結果的 JSONL 文件

使用方式：
  python -m duduclaw.memory_eval.build_golden_qa \\
    --input  wiki/eval/golden-qa-set.jsonl  \\  (或從 markdown 提取的 JSONL)
    --output memory_eval/data/golden_qa_set.jsonl \\
    --agent-id duduclaw-eng-memory \\
    --seed-run

環境變數：
  DUDUCLAW_API_URL  — DuDuClaw API base URL（例：http://localhost:8080）
  DUDUCLAW_API_KEY  — DuDuClaw API key

W21 Sprint 實作 — ENG-MEMORY
任務：0b5c478e-729d-46f7-9408-a2f281c605f4
"""
from __future__ import annotations

import argparse
import asyncio
import json
import logging
import sys
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Optional

from .client import build_client
from .config import EvalConfig

logger = logging.getLogger(__name__)

DEFAULT_SIMILARITY_THRESHOLD = 0.75
DEFAULT_SEARCH_LIMIT = 10


@dataclass
class GoldenQAPair:
    id:                  str
    query:               str
    relevant_memory_ids: list[str]
    source:              str
    created:             str
    category:            Optional[str] = None
    needs_manual_review: bool = False


def load_jsonl(path: Path) -> list[GoldenQAPair]:
    """從 JSONL 文件載入 Golden QA Set（支援 markdown 提取輸出）"""
    pairs: list[GoldenQAPair] = []
    with open(path, encoding="utf-8") as f:
        for lineno, line in enumerate(f, 1):
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            try:
                data = json.loads(line)
                pairs.append(GoldenQAPair(**{
                    k: v for k, v in data.items()
                    if k in GoldenQAPair.__dataclass_fields__
                }))
            except (json.JSONDecodeError, TypeError) as e:
                logger.warning("Line %d: parse error — %s", lineno, e)
    return pairs


def save_jsonl(pairs: list[GoldenQAPair], path: Path) -> None:
    """將 Golden QA Set 寫入 JSONL 文件"""
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        for pair in pairs:
            d = {
                "id":                  pair.id,
                "query":               pair.query,
                "relevant_memory_ids": pair.relevant_memory_ids,
                "source":              pair.source,
                "created":             pair.created,
                "category":            pair.category,
            }
            if pair.needs_manual_review:
                d["needs_manual_review"] = True
            f.write(json.dumps(d, ensure_ascii=False) + "\n")
    logger.info("Saved %d QA pairs to %s", len(pairs), path)


async def run_seed_run(
    pairs: list[GoldenQAPair],
    config: EvalConfig,
    similarity_threshold: float = DEFAULT_SIMILARITY_THRESHOLD,
    search_limit: int = DEFAULT_SEARCH_LIMIT,
    force_reseed: bool = False,
) -> tuple[list[GoldenQAPair], dict]:
    """
    對 relevant_memory_ids 為空的查詢執行 seed run。

    Seed Run 邏輯（依規格 §3.3）：
    1. 對每個空的 query 執行 memory_search(query, limit=search_limit)
    2. 取 similarity >= similarity_threshold 的結果 ID
    3. 有結果 → 填入 relevant_memory_ids
    4. 無結果 → 標記 needs_manual_review=True

    Args:
        pairs:                Golden QA Set 清單
        config:               EvalConfig（含 agent_id, db_dsn）
        similarity_threshold: 相關性門檻（預設 0.75）
        search_limit:         每次搜尋的最大回傳數（預設 10）
        force_reseed:         True = 強制對已有結果的 pair 重新 seed

    Returns:
        (updated_pairs, stats_dict)
    """
    client = build_client(config)

    stats = {
        "total":            len(pairs),
        "seeded":           0,
        "already_seeded":   0,
        "no_results":       0,
        "errors":           0,
    }

    updated: list[GoldenQAPair] = []

    for pair in pairs:
        # 已有結果且非強制重 seed → 跳過
        if pair.relevant_memory_ids and not force_reseed:
            stats["already_seeded"] += 1
            updated.append(pair)
            continue

        try:
            results = await client.search(
                query=pair.query,
                limit=search_limit,
            )

            relevant_ids = [
                r.memory_id
                for r in results
                if r.similarity >= similarity_threshold
            ]

            if relevant_ids:
                pair.relevant_memory_ids = relevant_ids
                pair.needs_manual_review = False
                stats["seeded"] += 1
                logger.debug(
                    "Seeded %s: %d relevant IDs (threshold=%.2f)",
                    pair.id, len(relevant_ids), similarity_threshold,
                )
            else:
                pair.relevant_memory_ids = []
                pair.needs_manual_review = True
                stats["no_results"] += 1
                logger.debug(
                    "No results for %s: query=%r (threshold=%.2f)",
                    pair.id, pair.query[:50], similarity_threshold,
                )

        except Exception as e:
            logger.error("Error seeding %s: %s", pair.id, e)
            stats["errors"] += 1
            pair.needs_manual_review = True

        updated.append(pair)

    return updated, stats


def print_stats(stats: dict, output_path: Path) -> None:
    """打印 seed run 統計報告"""
    total = stats["total"]
    seeded = stats["seeded"]
    already = stats["already_seeded"]
    no_results = stats["no_results"]
    errors = stats["errors"]

    coverage = (seeded + already) / total * 100 if total > 0 else 0.0
    needs_review = no_results + errors

    print("\n=== Seed Run 統計報告 ===")
    print(f"總計 QA pair:      {total}")
    print(f"本次新 seed:       {seeded}")
    print(f"已有結果（跳過）:  {already}")
    print(f"無相關記憶:        {no_results}  → needs_manual_review=True")
    print(f"執行錯誤:          {errors}")
    print(f"有效覆蓋率:        {coverage:.1f}%")
    print(f"需人工審核:        {needs_review}")
    print(f"輸出路徑:          {output_path}")
    print("========================\n")


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Golden QA Set Seed Run — 自動填入 relevant_memory_ids",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
範例：
  python -m duduclaw.memory_eval.build_golden_qa \\
    --input wiki/eval/golden-qa-set.jsonl \\
    --output memory_eval/data/golden_qa_set.jsonl \\
    --agent-id duduclaw-eng-memory \\
    --seed-run

  # 強制重新 seed 所有 pair（含已有結果）
  python -m duduclaw.memory_eval.build_golden_qa \\
    --input memory_eval/data/golden_qa_set.jsonl \\
    --output memory_eval/data/golden_qa_set.jsonl \\
    --agent-id duduclaw-eng-memory \\
    --seed-run \\
    --force-reseed

  # 調整相似度門檻（預設 0.75）
  python -m duduclaw.memory_eval.build_golden_qa \\
    --input memory_eval/data/golden_qa_set.jsonl \\
    --output memory_eval/data/golden_qa_set.jsonl \\
    --agent-id duduclaw-eng-memory \\
    --seed-run \\
    --similarity-threshold 0.80
""",
    )
    parser.add_argument(
        "--input",
        required=True,
        type=Path,
        help="輸入 JSONL 文件路徑（可直接使用 wiki/eval/golden-qa-set.jsonl）",
    )
    parser.add_argument(
        "--output",
        required=True,
        type=Path,
        help="輸出 JSONL 文件路徑",
    )
    parser.add_argument(
        "--agent-id",
        required=True,
        help="DuDuClaw Agent ID（例：duduclaw-eng-memory）",
    )
    parser.add_argument(
        "--seed-run",
        action="store_true",
        help="執行 seed run（需設定 DUDUCLAW_API_URL / DUDUCLAW_API_KEY）",
    )
    parser.add_argument(
        "--force-reseed",
        action="store_true",
        help="強制對已有 relevant_memory_ids 的 pair 重新 seed",
    )
    parser.add_argument(
        "--similarity-threshold",
        type=float,
        default=DEFAULT_SIMILARITY_THRESHOLD,
        help=f"相關性門檻（預設 {DEFAULT_SIMILARITY_THRESHOLD}）",
    )
    parser.add_argument(
        "--search-limit",
        type=int,
        default=DEFAULT_SEARCH_LIMIT,
        help=f"每次搜尋的最大回傳數（預設 {DEFAULT_SEARCH_LIMIT}）",
    )
    parser.add_argument(
        "--stats-only",
        action="store_true",
        help="僅顯示統計（不執行 seed run，不寫入輸出）",
    )
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="顯示詳細 debug log",
    )

    args = parser.parse_args(argv)

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s [%(levelname)s] %(name)s: %(message)s",
    )

    # 載入輸入
    if not args.input.exists():
        logger.error("Input file not found: %s", args.input)
        return 1

    pairs = load_jsonl(args.input)
    logger.info("Loaded %d QA pairs from %s", len(pairs), args.input)

    # stats-only 模式
    if args.stats_only:
        empty_count = sum(1 for p in pairs if not p.relevant_memory_ids)
        filled_count = len(pairs) - empty_count
        print(f"\n=== Golden QA Set 統計 ===")
        print(f"總計：           {len(pairs)}")
        print(f"已填入 IDs：     {filled_count}")
        print(f"待 seed run：   {empty_count}")
        coverage = filled_count / len(pairs) * 100 if pairs else 0.0
        print(f"覆蓋率：        {coverage:.1f}%")
        print(f"=============================\n")
        return 0

    if not args.seed_run:
        # 僅複製（不 seed）
        logger.info("--seed-run not specified; copying input to output as-is.")
        save_jsonl(pairs, args.output)
        return 0

    # 執行 seed run
    logger.info(
        "Starting seed run: threshold=%.2f, search_limit=%d, force=%s",
        args.similarity_threshold, args.search_limit, args.force_reseed,
    )

    config = EvalConfig(agent_id=args.agent_id)
    updated_pairs, stats = asyncio.run(
        run_seed_run(
            pairs=pairs,
            config=config,
            similarity_threshold=args.similarity_threshold,
            search_limit=args.search_limit,
            force_reseed=args.force_reseed,
        )
    )

    save_jsonl(updated_pairs, args.output)
    print_stats(stats, args.output)

    # 若有錯誤則回傳非零
    if stats["errors"] > 0:
        logger.warning("%d errors occurred during seed run", stats["errors"])
        return 2

    return 0


if __name__ == "__main__":
    sys.exit(main())
